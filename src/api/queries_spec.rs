//! Specification for resolving queries into functions and results.

use super::*;
use crate::backend::Backend;
use crate::config::Config;
use crate::engine::Piece;
use crate::python::Type;

fn column(name: &str, data_type: &str, not_null: bool) -> plugin::Column {
    plugin::Column {
        name: name.into(),
        not_null,
        r#type: Some(plugin::Identifier {
            name: data_type.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn of_authors(mut column: plugin::Column) -> plugin::Column {
    column.table = Some(plugin::Identifier {
        name: "authors".into(),
        ..Default::default()
    });
    column
}

/// The columns of `authors`: `id bigint NOT NULL, name text NOT NULL, bio text`.
fn authors() -> Vec<plugin::Column> {
    vec![
        column("id", "pg_catalog.int8", true),
        column("name", "text", true),
        column("bio", "text", false),
    ]
}

fn param(number: i32, column: plugin::Column) -> plugin::Parameter {
    plugin::Parameter {
        number,
        column: Some(column),
    }
}

fn query(cmd: &str, text: &str) -> plugin::Query {
    plugin::Query {
        name: "GetAuthor".into(),
        filename: "authors.sql".into(),
        cmd: cmd.into(),
        text: text.into(),
        ..Default::default()
    }
}

fn try_resolve_all(queries: Vec<plugin::Query>) -> Result<Vec<Module>> {
    try_resolve_on("postgresql", queries)
}

/// `queries`, resolved on `engine` against the `authors` catalog.
fn try_resolve_on(engine: &str, queries: Vec<plugin::Query>) -> Result<Vec<Module>> {
    try_resolve_with(engine, queries, &Config::of(Backend::SqlAlchemy))
}

/// The same, as `config` asks.
fn try_resolve_with(
    engine: &str,
    queries: Vec<plugin::Query>,
    config: &Config,
) -> Result<Vec<Module>> {
    let request = plugin::GenerateRequest {
        settings: Some(plugin::Settings {
            engine: engine.into(),
            ..Default::default()
        }),
        catalog: Some(plugin::Catalog {
            default_schema: "public".into(),
            // The catalog's types are PostgreSQL's.
            schemas: (engine == "postgresql")
                .then(|| plugin::Schema {
                    name: "public".into(),
                    tables: vec![plugin::Table {
                        rel: Some(plugin::Identifier {
                            name: "authors".into(),
                            ..Default::default()
                        }),
                        columns: authors(),
                        ..Default::default()
                    }],
                    enums: vec![plugin::Enum {
                        name: "mood".into(),
                        vals: vec!["happy".into()],
                        ..Default::default()
                    }],
                    ..Default::default()
                })
                .into_iter()
                .collect(),
            ..Default::default()
        }),
        queries,
        ..Default::default()
    };
    crate::api::resolve(&request, config).map(|api| api.modules)
}

fn try_resolve(query: plugin::Query) -> Result<Query> {
    try_resolve_all(vec![query]).map(|mut modules| modules.remove(0).queries.remove(0))
}

fn resolve(query: plugin::Query) -> Query {
    try_resolve(query).unwrap()
}

fn error(query: plugin::Query) -> String {
    format!("{:#}", try_resolve(query).unwrap_err())
}

fn returning(cmd: &str, columns: Vec<plugin::Column>) -> Command {
    resolve(plugin::Query {
        columns,
        ..query(cmd, "SELECT 1")
    })
    .command
}

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
    assert_eq!(copy.rows.as_deref(), Some("GetAuthorParams"));
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

// -- Parameters --------------------------------------------------------------

fn params(params: Vec<plugin::Parameter>, text: &str) -> Query {
    resolve(plugin::Query {
        params,
        ..query(":exec", text)
    })
}

#[test]
fn placeholders_become_the_parameters_python_names() {
    let query = params(
        vec![
            param(1, column("from", "text", true)),
            param(2, column("", "int4", true)),
        ],
        "SELECT $1, $2::int, $1",
    );
    let names: Vec<&str> = query.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["from_", "column_2"]);
    let placeholders: Vec<&str> = (query.sql.0.iter())
        .filter_map(|piece| match piece {
            Piece::Param { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(placeholders, ["from_", "column_2", "from_"]);
    assert_eq!(query.sql.text(), "SELECT $1, $2::int, $1");
}

#[test]
fn a_parameter_name_the_user_chose_is_never_numbered() {
    // `"userId" = $1 AND uid = sqlc.arg('user_id')`: numbering would bind the
    // user's `user_id` to `$1`, with the same type, so a swap type-checks.
    let named = plugin::Column {
        is_named_param: true,
        ..column("user_id", "int4", false)
    };
    let error = error(plugin::Query {
        params: vec![param(1, column("userId", "int4", false)), param(2, named)],
        ..query(":exec", "SELECT $1, $2")
    });
    assert!(
        error
            .contains(r#"parameters "userId" and "user_id" both become the Python name "user_id""#),
        "{error}"
    );
}

#[test]
fn a_parameter_sqlc_named_after_an_expression_is_an_error() {
    // `@day::date AT TIME ZONE 'Europe/Amsterdam'`: sqlc replaced the whole
    // expression with `$1`, dropping the time zone, and named it so. With
    // 'UTC' the name is an identifier, but still a flattened function call;
    // `@x + 1` is named "+x".
    for name in [
        "pg_catalogtimezoneEurope/Amsterdamdaydate",
        "pg_catalogtimezoneUTCdaytimestamptz",
        "+x",
    ] {
        let named = plugin::Column {
            is_named_param: true,
            ..column(name, "timestamptz", true)
        };
        let error = error(plugin::Query {
            params: vec![param(1, named)],
            ..query(":exec", "SELECT $1")
        });
        assert!(
            error.contains(&format!(
                r#"sqlc named a parameter "{name}" after an expression"#
            )),
            "{error}"
        );
    }
    // What a column gave its name to is the column's, whatever it is.
    let query = params(vec![param(1, column("my col", "text", true))], "SELECT $1");
    assert_eq!(query.params[0].name, "my_col");
}

#[test]
fn a_repeated_parameter_name_is_numbered_as_sqlc_numbers_it() {
    let query = params(
        vec![
            param(1, column("issued_on", "date", false)),
            param(2, column("issued_on", "date", false)),
            param(3, column("issued_on", "date", false)),
        ],
        "SELECT $1, $2, $3",
    );
    let names: Vec<&str> = query.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["issued_on", "issued_on_2", "issued_on_3"]);
}

#[test]
fn a_parameter_is_optional_exactly_when_sqlc_says_it_admits_null() {
    let query = params(
        vec![
            param(1, column("a", "text", true)),
            param(2, column("b", "text", false)),
        ],
        "SELECT $1, $2",
    );
    let optional: Vec<bool> = query.params.iter().map(|p| p.ty.optional).collect();
    assert_eq!(optional, [false, true]);
}

#[test]
fn a_json_parameter_keeps_which_json_type_it_is() {
    let query = params(
        vec![
            param(1, column("a", "json", false)),
            param(2, column("b", "pg_catalog.jsonb", false)),
            param(3, column("c", "text", false)),
        ],
        "SELECT $1, $2, $3",
    );
    let types: Vec<&Type> = query.params.iter().map(|p| &p.ty.element).collect();
    assert_eq!(types, [&Type::JSON, &Type::JSONB, &Type::STR]);
}

#[test]
fn an_unprobed_parameter_is_an_error_naming_its_number_and_name() {
    let mut array = column("docs", "jsonb", true);
    (array.is_array, array.array_dims) = (true, 1);
    let unknown = |column| {
        error(plugin::Query {
            params: vec![param(2, column)],
            ..query(":exec", "SELECT $2")
        })
    };
    assert!(
        unknown(column("p", "point", true)).contains(
            r#"parameter $2 ("p") has type "point", which has no verified Python mapping"#
        )
    );
    assert!(unknown(array).contains(r#"parameter $2 ("docs") is an array of "jsonb""#));
    // A parameter's cast is written on the placeholder, not coalesced.
    let uninferred = unknown(column("x", "", false));
    assert!(
        uninferred.contains(
            r#"parameter $2 ("x") has a type sqlc could not infer; add a cast, as @x::<type>, or sqlc.narg('x')::<type> if it can be NULL"#
        ),
        "{uninferred}"
    );
}

#[test]
fn a_parameter_typed_by_a_guessed_overload_asks_for_a_cast() {
    // sqlc types `abs(@x)` by the first overload of `abs` it lists, `bigint`,
    // and `date_trunc('month', @d)` by `interval`: `x: int` would push a
    // caller to drop an amount's cents. LIMIT and OFFSET are sqlc's own
    // `integer`, which is right.
    for (name, data_type) in [("x", "bigint"), ("d", "interval")] {
        let error = error(plugin::Query {
            params: vec![param(1, column(name, data_type, true))],
            ..query(":exec", "SELECT $1")
        });
        assert!(
            error.contains(&format!(
                r#"parameter $1 ("{name}") has a type sqlc could not infer; add a cast, as @{name}::<type>"#
            )),
            "{error}"
        );
    }
    let limit = params(
        vec![param(1, column("lim", "integer", true))],
        "SELECT 1 LIMIT $1",
    );
    assert_eq!(limit.params[0].ty.element, Type::INT);
}

#[test]
fn sqlc_slice_is_an_error_that_says_what_postgresql_uses_instead() {
    let mut ids = column("ids", "int4", true);
    ids.is_sqlc_slice = true;
    let error = error(plugin::Query {
        params: vec![param(1, ids)],
        ..query(":exec", "SELECT $1")
    });
    assert!(
        error.contains("sqlc.slice is MySQL/SQLite only; on PostgreSQL use = ANY(@ids::type[])"),
        "{error}"
    );
}

// -- Results -----------------------------------------------------------------

#[test]
fn the_commands_that_return_no_rows_ignore_any_columns() {
    assert_eq!(returning(":exec", vec![]), Command::Exec);
    assert_eq!(returning(":execrows", vec![]), Command::ExecRows);
    assert_eq!(returning(":execresult", vec![]), Command::ExecResult);
    // A `:copyfrom` is an `:exec` of each row.
    let copy = resolve(self::query(":copyfrom", "SELECT 1"));
    assert_eq!(copy.command, Command::Exec);
    assert_eq!(copy.rows.as_deref(), Some("GetAuthorParams"));
}

#[test]
fn a_single_column_is_returned_bare() {
    let id = FieldType {
        element: Type::INT,
        array_dims: 0,
        optional: false,
    };
    assert_eq!(
        returning(":one", vec![column("id", "int8", true)]),
        Command::One(Shape::Scalar(id.clone()))
    );
    assert_eq!(
        returning(":many", vec![column("bio", "int8", false)]),
        Command::Many(Shape::Scalar(FieldType {
            optional: true,
            ..id
        }))
    );
}

#[test]
fn a_nullable_or_json_single_column_is_returned_bare_since_no_row_raises() {
    // `None` can only mean NULL (or JSON `null`): a missing row raises.
    for (name, ty, not_null) in [("bio", "text", false), ("body", "jsonb", true)] {
        for cmd in [":one", ":many"] {
            let command = returning(cmd, vec![column(name, ty, not_null)]);
            assert!(
                matches!(
                    command,
                    Command::One(Shape::Scalar(_)) | Command::Many(Shape::Scalar(_))
                ),
                "{cmd} {ty}: {command:?}"
            );
        }
    }
}

#[test]
fn a_result_of_exactly_a_tables_columns_reuses_its_model() {
    let columns = authors().into_iter().map(of_authors).collect();
    assert_eq!(
        returning(":many", columns),
        Command::Many(Shape::Model("Author".into()))
    );
}

#[test]
fn a_result_that_differs_from_the_table_in_any_way_gets_its_own_row() {
    let mut nullable = authors();
    nullable[0].not_null = false;
    let mut renamed = authors();
    renamed[2].name = "biography".into();
    let mut unowned = authors().into_iter().map(of_authors).collect::<Vec<_>>();
    unowned[1].table = None;
    for columns in [
        authors()[..2].to_vec(),
        nullable,
        renamed,
        authors().into_iter().rev().collect(),
    ] {
        let columns: Vec<_> = columns.into_iter().map(of_authors).collect();
        assert!(matches!(
            returning(":one", columns),
            Command::One(Shape::Row(..))
        ));
    }
    assert!(matches!(
        returning(":one", unowned),
        Command::One(Shape::Row(..))
    ));
}

#[test]
fn repeated_result_columns_are_numbered_and_unnamed_ones_named_by_position() {
    let Command::Many(Shape::Row(_, fields)) = returning(
        ":many",
        vec![
            column("id", "int8", true),
            column("id", "int8", true),
            column("", "text", true),
        ],
    ) else {
        panic!("not a row");
    };
    let names: Vec<&str> = (fields.iter())
        .map(|field| match field {
            RowField::Column(field) => field.name.as_str(),
            RowField::Embed { name, .. } => name.as_str(),
        })
        .collect();
    assert_eq!(names, ["id", "id_2", "column_3"]);
}

#[test]
fn an_embed_is_a_field_of_its_model_over_the_tables_columns() {
    let embed = plugin::Column {
        name: "authors".into(),
        embed_table: Some(plugin::Identifier {
            name: "authors".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let query = resolve(plugin::Query {
        columns: vec![embed, column("total", "int8", true)],
        ..query(":one", "SELECT 1")
    });
    let Command::One(Shape::Row(_, fields)) = &query.command else {
        panic!("not a row");
    };
    assert_eq!(
        fields[0],
        RowField::Embed {
            name: "author".into(),
            model: "Author".into(),
            width: 3
        }
    );
    let flat: Vec<&str> = query.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(flat, ["id", "name", "bio", "total"]);
}

#[test]
fn an_embed_of_a_table_the_catalog_lacks_is_an_error() {
    let embed = plugin::Column {
        name: "ghosts".into(),
        embed_table: Some(plugin::Identifier {
            name: "ghosts".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(
        error(plugin::Query {
            columns: vec![embed],
            ..query(":one", "SELECT 1")
        })
        .ends_with("sqlc.embed(ghosts) names no table")
    );
}

// -- MySQL and SQLite ----------------------------------------------------------

/// `query` on `engine`, its only query.
fn on(engine: &str, query: plugin::Query) -> Result<Query> {
    try_resolve_on(engine, vec![query]).map(|mut modules| modules.remove(0).queries.remove(0))
}

fn named(number: i32, name: &str, data_type: &str, not_null: bool) -> plugin::Parameter {
    param(
        number,
        plugin::Column {
            is_named_param: true,
            ..column(name, data_type, not_null)
        },
    )
}

#[test]
fn a_name_the_user_chose_is_one_parameter_wherever_mysql_binds_it() {
    // MySQL's `?` cannot be bound twice, so sqlc reports each use.
    let resolved = on(
        "mysql",
        plugin::Query {
            params: vec![named(1, "q", "text", true), named(2, "q", "text", false)],
            ..query(":exec", "SELECT 1 WHERE a = ? OR b = ?")
        },
    )
    .unwrap();
    let names: Vec<&str> = resolved.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["q"]);
    let bound: Vec<&str> = (resolved.sql.0.iter())
        .filter_map(|piece| match piece {
            Piece::Param { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(bound, ["q", "q"]);
    // Either use admitting NULL makes it optional.
    assert!(resolved.params[0].ty.optional);
    // Two uses of two types cannot be one parameter.
    let error = on(
        "mysql",
        plugin::Query {
            params: vec![named(1, "q", "text", true), named(2, "q", "int", true)],
            ..query(":exec", "SELECT 1 WHERE a = ? OR b = ?")
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("is used as str and as int"),
        "{error:#}"
    );
}

#[test]
fn a_slice_is_a_list_of_its_type_bound_one_placeholder_per_item() {
    let slice = param(
        1,
        plugin::Column {
            is_sqlc_slice: true,
            ..column("ids", "integer", true)
        },
    );
    let resolved = on(
        "sqlite",
        plugin::Query {
            params: vec![slice.clone()],
            ..query(":exec", "SELECT 1 WHERE id IN (/*SLICE:ids*/?)")
        },
    )
    .unwrap();
    assert!(resolved.sql.is_slice("ids"));
    assert_eq!(
        resolved.params[0]
            .ty
            .annotation(crate::python::Scope::Queries),
        "list[int]"
    );
    assert!(resolved.sql.0.contains(&Piece::Slice {
        marker: "/*SLICE:ids*/".into(),
        placeholder: "?".into(),
        name: "ids".into(),
    }));
    let error = on(
        "postgresql",
        plugin::Query {
            params: vec![slice],
            ..query(":exec", "SELECT 1 WHERE id IN ($1)")
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("sqlc.slice is MySQL/SQLite only"));
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
        assert_eq!(resolved.rows.as_deref(), Some("GetAuthorParams"));
    }
}

#[test]
fn a_rename_that_makes_two_chosen_parameters_one_name_is_an_error() {
    let mut config = Config::of(Backend::SqlAlchemy);
    config.rename = [("b".to_string(), "a".to_string())].into();
    let error = try_resolve_with(
        "mysql",
        vec![plugin::Query {
            params: vec![named(1, "a", "text", true), named(2, "b", "text", true)],
            ..query(":exec", "SELECT 1 WHERE x = ? AND y = ?")
        }],
        &config,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("both become the Python name \"a\""),
        "{error:#}"
    );
}

#[test]
fn a_slice_of_a_nullable_column_is_a_list_never_none() {
    let slice = param(
        1,
        plugin::Column {
            is_sqlc_slice: true,
            ..column("ids", "integer", false)
        },
    );
    let resolved = on(
        "sqlite",
        plugin::Query {
            params: vec![slice],
            ..query(":exec", "SELECT 1 WHERE id IN (/*SLICE:ids*/?)")
        },
    )
    .unwrap();
    let ty = &resolved.params[0].ty;
    assert_eq!(ty.annotation(crate::python::Scope::Queries), "list[int]");
}

#[test]
fn arithmetic_on_mysql_and_sqlite_asks_for_a_cast() {
    // sqlc types `a + b` as `int`, and MySQL's `1.0` as `float`, of no table,
    // whatever the operands; a cast it spells as declared.
    for (engine, data_type) in [("mysql", "int"), ("mysql", "float"), ("sqlite", "int")] {
        let error = on(
            engine,
            plugin::Query {
                columns: vec![column("total", data_type, true)],
                ..query(":one", "SELECT a + b AS total FROM t")
            },
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("typed by its form"),
            "{engine} {data_type}: {error:#}"
        );
    }
    let cast = on(
        "sqlite",
        plugin::Query {
            columns: vec![column("total", "INTEGER", true)],
            ..query(":one", "SELECT CAST(a + b AS INTEGER) AS total FROM t")
        },
    );
    assert!(cast.is_ok());
}
