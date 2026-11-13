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
    let request = plugin::GenerateRequest {
        settings: Some(plugin::Settings {
            engine: "postgresql".into(),
            ..Default::default()
        }),
        catalog: Some(plugin::Catalog {
            default_schema: "public".into(),
            schemas: vec![plugin::Schema {
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
            }],
            ..Default::default()
        }),
        queries,
        ..Default::default()
    };
    crate::api::resolve(&request, &Config::of(Backend::SqlAlchemy)).map(|api| api.modules)
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
            query.class()
        ),
        ("get_author", "_GET_AUTHOR", None)
    );
    assert_eq!(
        returning(":copyfrom", vec![]),
        Command::CopyFrom("GetAuthorParams".into())
    );
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
fn commands_without_a_postgresql_meaning_are_errors_that_say_what_to_use() {
    assert!(
        error(query(":execlastid", ""))
            .contains(":execlastid is not supported; use INSERT ... RETURNING with :one")
    );
    for batch in [":batchexec", ":batchmany", ":batchone"] {
        let error = error(query(batch, ""));
        assert!(
            error.contains(&format!(
                "{batch} is pgx-only; use :copyfrom for bulk inserts"
            )),
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
        let error = error(plugin::Query {
            columns: vec![column],
            ..query(":one", "SELECT 1")
        });
        assert!(
            error.contains(&format!(
                r#"column "{name}" has a type sqlc could not infer; add a cast, as coalesce("#
            )),
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
            Piece::Text(_) => None,
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
    assert_eq!(
        returning(":copyfrom", vec![]),
        Command::CopyFrom("GetAuthorParams".into())
    );
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
