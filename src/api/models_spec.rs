//! Specification for resolving a catalog into classes and annotations.

use super::*;
use crate::api::Api;
use crate::backend::Backend;
use crate::config::{Config, ModelConfig};

// -- Annotations -------------------------------------------------------------

fn field(element: Type, array_dims: usize, optional: bool) -> FieldType {
    FieldType {
        element,
        array_dims,
        optional,
    }
}

fn annotation(ty: &FieldType) -> String {
    ty.describe(Scope::Models)
}

#[test]
fn a_plain_column_annotates_as_its_element_type() {
    assert_eq!(annotation(&field(Type::INT, 0, false)), "int");
    assert_eq!(annotation(&field(Type::DATE, 0, false)), "dt.date");
    assert_eq!(
        annotation(&field(Type::CIDR, 0, false)),
        "IPv4Network | IPv6Network"
    );
}

#[test]
fn a_nullable_column_is_optional() {
    // PEP 604, Python 3.10's spelling: no `typing.Optional` to import.
    assert_eq!(annotation(&field(Type::STR, 0, true)), "str | None");
    assert_eq!(
        annotation(&field(Type::CIDR, 0, true)),
        "IPv4Network | IPv6Network | None"
    );
}

#[test]
fn a_generated_class_is_bare_in_the_models_module_and_qualified_outside_it() {
    let status = field(Type::Enum("Status".into()), 0, false);
    assert_eq!(status.describe(Scope::Models), "Status");
    assert_eq!(status.describe(Scope::Queries), "models.Status");
}

#[test]
fn each_array_dimension_adds_a_list() {
    assert_eq!(annotation(&field(Type::INT, 1, false)), "list[int]");
    assert_eq!(
        annotation(&field(Type::INT, 3, false)),
        "list[list[list[int]]]"
    );
}

#[test]
fn a_nullable_array_is_an_optional_list_not_a_list_of_optionals() {
    // The column may be absent; its elements are not independently nullable.
    assert_eq!(annotation(&field(Type::INT, 1, true)), "list[int] | None");
}

// -- Resolving a catalog -----------------------------------------------------

fn resolve(schemas: Vec<plugin::Schema>, config: &ModelConfig) -> Api {
    resolve_with(schemas, config).unwrap()
}

fn try_resolve(schemas: Vec<plugin::Schema>) -> Result<Api> {
    resolve_with(schemas, &ModelConfig::default())
}

fn resolve_with(schemas: Vec<plugin::Schema>, config: &ModelConfig) -> Result<Api> {
    let config = Config {
        models: config.clone(),
        ..Config::of(Backend::SqlAlchemy)
    };
    resolve_options(schemas, &config)
}

fn resolve_options(schemas: Vec<plugin::Schema>, config: &Config) -> Result<Api> {
    let request = plugin::GenerateRequest {
        settings: Some(plugin::Settings {
            engine: "postgresql".into(),
            ..plugin::Settings::default()
        }),
        catalog: Some(plugin::Catalog {
            default_schema: "public".into(),
            schemas,
            ..plugin::Catalog::default()
        }),
        ..plugin::GenerateRequest::default()
    };
    crate::api::resolve(&request, config)
}

fn sql_schema(name: &str, tables: Vec<plugin::Table>, enums: Vec<plugin::Enum>) -> plugin::Schema {
    plugin::Schema {
        name: name.into(),
        tables,
        enums,
        ..plugin::Schema::default()
    }
}

fn table(name: &str, columns: Vec<plugin::Column>) -> plugin::Table {
    plugin::Table {
        rel: Some(plugin::Identifier {
            name: name.into(),
            ..plugin::Identifier::default()
        }),
        columns,
        ..plugin::Table::default()
    }
}

fn column(name: &str, data_type: &str) -> plugin::Column {
    plugin::Column {
        name: name.into(),
        not_null: true,
        r#type: Some(plugin::Identifier {
            name: data_type.into(),
            ..plugin::Identifier::default()
        }),
        ..plugin::Column::default()
    }
}

fn public(tables: Vec<plugin::Table>) -> Vec<plugin::Schema> {
    vec![sql_schema("public", tables, vec![])]
}

fn model_names(api: &Api) -> Vec<&str> {
    api.models.iter().map(|m| m.name.as_str()).collect()
}

#[test]
fn a_table_becomes_a_singular_class_name() {
    let resolved = resolve(
        public(vec![
            table("authors", vec![column("id", "int8")]),
            table("test_scores", vec![column("score", "int4")]),
        ]),
        &ModelConfig::default(),
    );

    assert_eq!(model_names(&resolved), ["Author", "TestScore"]);
}

#[test]
fn exact_table_names_keeps_the_table_name_verbatim() {
    let config = ModelConfig {
        exact_table_names: true,
        ..ModelConfig::default()
    };
    let resolved = resolve(public(vec![table("authors", vec![])]), &config);

    assert_eq!(model_names(&resolved), ["Authors"]);
}

#[test]
fn inflection_exclude_keeps_the_tables_it_names_verbatim_in_any_case() {
    let config = ModelConfig {
        inflection_exclude: vec!["Media".into()],
        ..ModelConfig::default()
    };
    let tables = || public(vec![table("authors", vec![]), table("media", vec![])]);

    assert_eq!(
        model_names(&resolve(tables(), &config)),
        ["Author", "Media"]
    );
    assert_eq!(
        model_names(&resolve(tables(), &ModelConfig::default())),
        ["Author", "Medium"]
    );
}

#[test]
fn a_table_outside_the_default_schema_is_qualified() {
    // Two `events` tables in one catalog must not collapse to one class.
    let resolved = resolve(
        vec![
            sql_schema("public", vec![table("events", vec![])], vec![]),
            sql_schema("analytics", vec![table("events", vec![])], vec![]),
        ],
        &ModelConfig::default(),
    );

    assert_eq!(model_names(&resolved), ["AnalyticsEvent", "Event"]);
}

#[test]
fn enums_are_sorted_by_class_name_whatever_the_catalogs_order() {
    let named = |name: &str| plugin::Enum {
        name: name.into(),
        vals: vec!["a".into()],
        comment: String::new(),
    };
    let resolved = resolve(
        vec![sql_schema(
            "public",
            vec![],
            vec![named("zone"), named("area")],
        )],
        &ModelConfig::default(),
    );

    let names: Vec<&str> = resolved.enums.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["Area", "Zone"]);
}

#[test]
fn an_enum_column_resolves_to_the_generated_enum_class() {
    let resolved = resolve(
        vec![sql_schema(
            "public",
            vec![table("jobs", vec![column("status", "job_status")])],
            vec![plugin::Enum {
                name: "job_status".into(),
                vals: vec!["queued".into(), "in-progress".into()],
                comment: String::new(),
            }],
        )],
        &ModelConfig::default(),
    );

    assert_eq!(
        resolved.models[0].fields[0].ty.element,
        Type::Enum("JobStatus".into())
    );
    assert_eq!(resolved.enums[0].name, "JobStatus");

    let members: Vec<&str> = resolved.enums[0]
        .members
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    assert_eq!(members, ["QUEUED", "IN_PROGRESS"]);
}

fn enum_column(data_type: &str, array_dims: i32) -> Result<FieldType> {
    let payment_method = || plugin::Enum {
        name: "payment_method".into(),
        vals: vec!["card".into()],
        comment: String::new(),
    };
    let column = plugin::Column {
        is_array: array_dims > 0,
        array_dims,
        ..column("method", data_type)
    };
    let resolved = try_resolve(vec![
        sql_schema(
            "public",
            vec![table("payments", vec![column])],
            vec![payment_method()],
        ),
        sql_schema("billing", vec![], vec![payment_method()]),
    ])?;
    Ok(resolved.models[0].fields[0].ty.clone())
}

#[test]
fn an_enum_is_found_in_the_schema_the_column_names_or_else_the_default_one() {
    // `docs/python-output.md` §5, rule 3: two schemas may declare one name.
    let class = |data_type| enum_column(data_type, 0).unwrap().element;
    assert_eq!(class("payment_method"), Type::Enum("PaymentMethod".into()));
    assert_eq!(
        class("public.payment_method"),
        Type::Enum("PaymentMethod".into())
    );
    assert_eq!(
        class("billing.payment_method"),
        Type::Enum("BillingPaymentMethod".into())
    );
    assert!(enum_column("sales.payment_method", 0).is_err());
}

#[test]
fn a_multi_dimensional_enum_array_is_an_error() {
    // No driver evidence for one, and `from_row` converts one dimension.
    assert!(enum_column("payment_method", 1).is_ok());
    let error = error_of(enum_column("payment_method", 2));
    assert!(error.contains("multi-dimensional"), "{error}");
}

#[test]
fn a_value_is_read_as_is_unless_it_holds_enum_labels() {
    // `docs/python-output.md` §4, rule 4: both drivers return labels as `str`.
    let row = || crate::python::expr("row[3]");
    let read = |ty: FieldType, scope| crate::python::python_source(&ty.read(row(), scope, false));
    let status = || Type::Enum("Status".into());
    assert_eq!(read(field(Type::DECIMAL, 1, true), Scope::Models), "row[3]");
    assert_eq!(
        read(field(status(), 0, false), Scope::Models),
        "Status(row[3])"
    );
    assert_eq!(
        read(field(status(), 0, true), Scope::Models),
        "None if row[3] is None else Status(row[3])"
    );
    assert_eq!(
        read(field(status(), 1, false), Scope::Queries),
        "[models.Status(v) for v in row[3]]"
    );
    assert_eq!(
        read(field(status(), 1, true), Scope::Models),
        "None if row[3] is None else [Status(v) for v in row[3]]"
    );
    // psycopg returns an array of enums as its text.
    assert_eq!(
        crate::python::python_source(&field(status(), 1, false).read(row(), Scope::Queries, true)),
        "models.Status.from_array(row[3])"
    );
}

#[test]
fn a_type_nothing_declares_is_an_error_naming_the_column() {
    // CLAUDE.md: never paper over an unknown with `Any`. A domain is one too:
    // sqlc sends its name, never its base type, so the error says what to
    // declare instead.
    let error =
        try_resolve(public(vec![table("odd", vec![column("value", "amount")])])).unwrap_err();

    assert_eq!(
        format!("{error:#}"),
        r#"table "odd": column "value" has type "amount", which has no verified Python mapping; name its Python type with an override (db_type "amount"), or, for a domain, declare the column with its base type and a CHECK constraint"#
    );
}

#[test]
fn an_array_the_drivers_do_not_load_as_a_list_is_an_error() {
    // `citext` is a `str`, but psycopg returns a `citext[]` as its text, and
    // asyncpg corrupts an empty item of a `tsvector[]`.
    for data_type in ["citext", "tsvector"] {
        let mut tags = column("tags", data_type);
        (tags.is_array, tags.array_dims) = (true, 1);
        let error = try_resolve(public(vec![table("posts", vec![tags])])).unwrap_err();
        let message = format!(r#"column "tags" is an array of "{data_type}", which "#);
        assert!(format!("{error:#}").contains(&message), "{error:#}");
    }
}

#[test]
fn a_view_column_sqlc_typed_by_a_guessed_overload_asks_for_a_cast() {
    // `CREATE VIEW v AS SELECT sum(amount) AS total ...` on a numeric column:
    // sqlc types `total` by the first overload of `sum` it lists, in SQL's
    // standard spelling, and PostgreSQL returns a Decimal. A declared or cast
    // column arrives as PostgreSQL's own name (`pg_catalog.int8`).
    for guess in ["bigint", "double precision", "numeric", "interval"] {
        let view = table("v_totals", vec![column("total", guess)]);
        let error = try_resolve(public(vec![view])).unwrap_err();
        assert!(
            format!("{error:#}").starts_with(
                r#"table "v_totals": column "total" has a type sqlc could not infer; add a cast"#
            ),
            "{guess}: {error:#}"
        );
    }
}

#[test]
fn a_composite_type_column_is_an_error_until_composites_get_classes() {
    // Annotating it with a class that is never generated would make importing
    // the module raise `NameError`.
    let point = plugin::CompositeType {
        name: "point_type".into(),
        comment: String::new(),
    };
    let error = try_resolve(vec![plugin::Schema {
        composite_types: vec![point],
        ..sql_schema(
            "public",
            vec![table("paths", vec![column("start", "point_type")])],
            vec![],
        )
    }])
    .unwrap_err();

    assert!(format!("{error:#}").contains("point_type"), "{error:#}");
}

#[test]
fn a_column_that_admits_null_is_optional() {
    let nullable = plugin::Column {
        not_null: false,
        ..column("bio", "text")
    };
    let resolved = resolve(
        public(vec![table("authors", vec![nullable])]),
        &ModelConfig::default(),
    );

    assert!(resolved.models[0].fields[0].ty.optional);
}

#[test]
fn an_array_sqlc_gave_no_depth_is_an_error_not_a_guess() {
    // sqlc 1.31 sets a depth on every array column; one without is a request
    // this plugin has no evidence of.
    let tags = plugin::Column {
        is_array: true,
        ..column("tags", "text")
    };
    let error = try_resolve(public(vec![table("posts", vec![tags])])).unwrap_err();
    assert_eq!(
        format!("{error:#}"),
        r#"table "posts": column "tags" is an array, but sqlc reported no array depth for it"#
    );
}

// -- Names in one scope ------------------------------------------------------

fn try_field_names(columns: &[&str]) -> Result<Vec<String>> {
    let columns = columns.iter().map(|name| column(name, "text")).collect();
    let resolved = try_resolve(public(vec![table("t", columns)]))?;
    Ok((resolved.models[0].fields.iter())
        .map(|f| f.name.clone())
        .collect())
}

fn field_names(columns: &[&str]) -> Vec<String> {
    try_field_names(columns).unwrap()
}

fn try_member_names(values: &[&str]) -> Result<Vec<String>> {
    let sql_enum = plugin::Enum {
        name: "op".into(),
        vals: values.iter().map(|v| v.to_string()).collect(),
        comment: String::new(),
    };
    let resolved = try_resolve(vec![sql_schema("public", vec![], vec![sql_enum])])?;
    Ok((resolved.enums[0].members.iter())
        .map(|m| m.name.clone())
        .collect())
}

fn error_of<T: std::fmt::Debug>(result: Result<T>) -> String {
    format!("{:#}", result.unwrap_err())
}

#[test]
fn a_field_is_the_snake_case_spelling_of_its_column() {
    assert_eq!(
        field_names(&["id", "authorID", "parent        id", "Weird Col"]),
        ["id", "author_id", "parent_id", "weird_col"]
    );
}

#[test]
fn a_column_named_like_a_keyword_takes_a_trailing_underscore() {
    // PEP 8's convention; `class: str` would not parse.
    assert_eq!(
        field_names(&["class", "if", "None"]),
        ["class_", "if_", "none"]
    );
}

#[test]
fn a_column_named_like_a_name_generated_code_reads_takes_a_trailing_underscore() {
    // docs/python-output.md §3, rule 1, written out rather than read from
    // `RESERVED`, so a name dropped from it fails here. In a class body,
    // `str: str` makes every later `str` annotation mean the field to mypy and
    // pyright, and `classmethod` the decorator of `from_row`; every module the
    // generated code imports is a name it reads, and `conn`, `row`, `rows` and
    // `value` are its locals.
    let reserved = [
        "dataclasses",
        "dt",
        "enum",
        "models",
        "postgresql",
        "sqlalchemy",
        "bool",
        "bytes",
        "classmethod",
        "conn",
        "float",
        "from_row",
        "int",
        "list",
        "object",
        "row",
        "rows",
        "str",
        "value",
    ];
    let renamed: Vec<String> = reserved.iter().map(|name| format!("{name}_")).collect();
    assert_eq!(field_names(&reserved), renamed);
    // Classes are imported by their PascalCase name, which a snake_case field
    // cannot take, so `uuid` and `decimal` are free; `id` is §3, rule 2.
    assert_eq!(
        field_names(&["uuid", "decimal", "datetime", "id"]),
        ["uuid", "decimal", "datetime", "id"]
    );
}

#[test]
fn a_column_sqlc_left_unnamed_is_named_by_position_as_sqlc_does() {
    // sqlc's Go codegen names an unnamed column `column_<n>`, counting from one.
    assert_eq!(field_names(&["id", ""]), ["id", "column_2"]);
}

#[test]
fn a_column_with_no_usable_spelling_is_an_error_naming_it() {
    for name in ["?!", "1st", "größe", "ﬁle"] {
        let error = error_of(try_field_names(&["id", name, "file"]));
        assert!(
            error.contains(&format!("\"{name}\" does not become")),
            "{error}"
        );
    }
}

#[test]
fn columns_that_spell_alike_are_an_error_not_a_silent_rename() {
    // Either would silently replace the other in the class body.
    let error = error_of(try_field_names(&["user_id", "userId"]));
    assert!(
        error.contains(r#""user_id" and "userId" both become the Python name "user_id""#),
        "{error}"
    );
    let error = error_of(try_field_names(&["from", "from_"]));
    assert!(error.contains(r#"the Python name "from_""#), "{error}");
}

#[test]
fn an_enum_member_is_spelt_as_sqlc_spells_a_go_constant_then_upper_cased() {
    // `EnumReplace`: `-`, `/`, `:` and `_` become `_`, the rest is dropped.
    assert_eq!(
        try_member_names(&["direct-debit", "a/b:c", "inProgress", "x.y", "v1"]).unwrap(),
        ["DIRECT_DEBIT", "A_B_C", "INPROGRESS", "XY", "V1"]
    );
}

#[test]
fn an_enum_label_with_no_usable_spelling_is_an_error_naming_it() {
    // Nothing left; not an identifier; or a name `enum` would reserve
    // (`_x_`) or mangle (`__x`), or pyright and ruff call private (`_A`).
    for label in ["=", "", "1st", "_x_", "__x", "-a"] {
        let error = error_of(try_member_names(&["open", label]));
        assert!(
            error.contains(&format!("\"{label}\" does not become")),
            "{label}: {error}"
        );
    }
}

#[test]
fn enum_labels_that_spell_alike_are_an_error_not_a_numbered_member() {
    // A repeated member name is a `TypeError` when the enum class is created,
    // and Go's `value_<i>` fallback would be a member nobody could guess.
    let error = error_of(try_member_names(&["a-b", "a_b"]));
    assert!(
        error.contains(r#""a-b" and "a_b" both become the Python name "A_B""#),
        "{error}"
    );
}

#[test]
fn two_objects_that_would_share_a_class_name_are_an_error() {
    // sqlc's Go codegen refuses the same: "struct name conflicts with enum name".
    let request = vec![sql_schema(
        "public",
        vec![table("foos", vec![])],
        vec![plugin::Enum {
            name: "foo".into(),
            ..plugin::Enum::default()
        }],
    )];
    let error = try_resolve(request).unwrap_err();
    assert!(error.to_string().contains("`Foo`"), "{error}");
}

#[test]
fn a_table_with_no_usable_class_name_is_an_error() {
    for name in ["1st_place", "none", "!!!", "Ärger"] {
        let error = try_resolve(public(vec![table(name, vec![])])).unwrap_err();
        assert!(
            error.to_string().contains("not a usable Python class name"),
            "{name}: {error}"
        );
    }
}

#[test]
fn a_class_with_the_name_of_an_import_is_an_error() {
    // `class Sequence` would change what every `Sequence[Any]` means.
    let error = try_resolve(public(vec![table("sequences", vec![])])).unwrap_err();
    assert_eq!(
        error.to_string(),
        "`Sequence` is a name generated code imports; rename the table or type"
    );
    let decimal = plugin::Enum {
        name: "decimal".into(),
        ..plugin::Enum::default()
    };
    let error = try_resolve(vec![sql_schema("public", vec![], vec![decimal])]).unwrap_err();
    assert!(
        error.to_string().starts_with("`Decimal` is a name"),
        "{error}"
    );
}

// -- Overrides and renames ----------------------------------------------------

/// The options `{"backend": "sqlalchemy", "async": false, ...json}`.
fn options(json: &str) -> Config {
    let request = plugin::GenerateRequest {
        plugin_options: format!(r#"{{"backend": "sqlalchemy", "async": false, {json}}}"#)
            .into_bytes(),
        ..plugin::GenerateRequest::default()
    };
    Config::from_request(&request).unwrap()
}

/// The annotation of each field of the only model, by name.
fn annotations(api: &Api) -> Vec<(String, String)> {
    (api.models[0].fields.iter())
        .map(|f| (f.name.clone(), f.ty.describe(crate::python::Scope::Models)))
        .collect()
}

#[test]
fn an_override_names_the_type_of_a_column_or_of_every_column_of_a_type() {
    let mut path = column("path", "vector");
    path.not_null = false;
    // sqlc names each catalog column's table.
    let label = plugin::Column {
        table: Some(plugin::Identifier {
            name: "items".into(),
            ..plugin::Identifier::default()
        }),
        ..column("label", "text")
    };
    let schemas = || {
        vec![sql_schema(
            "public",
            vec![table(
                "items",
                vec![column("id", "pg_catalog.int8"), path.clone(), label.clone()],
            )],
            vec![],
        )]
    };
    // An unknown type is an error until an override names it.
    assert!(resolve_options(schemas(), &options(r#""overrides": []"#)).is_err());
    let config = options(
        r#""overrides": [
            {"db_type": "vector", "python_type": "str"},
            {"db_type": "int8", "python_type": "int"},
            {"column": "items.label", "python_type": "app.types.Label"}
        ]"#,
    );
    let api = resolve_options(schemas(), &config).unwrap();
    assert_eq!(
        annotations(&api),
        [
            ("id".into(), "int".into()),
            ("path".into(), "str | None".into()),
            ("label".into(), "app.types.Label".into()),
        ]
    );
    assert_eq!(config.modules(), ["app.types"]);
}

#[test]
fn an_override_is_an_annotation_whose_every_name_can_be_imported() {
    for bad in ["Label", "list[Label]", "1 + 2", "lambda: 0"] {
        let request = plugin::GenerateRequest {
            plugin_options: format!(
                r#"{{"backend": "sqlalchemy", "async": false, "overrides": [{{"db_type": "x", "python_type": "{bad}"}}]}}"#
            )
            .into_bytes(),
            ..plugin::GenerateRequest::default()
        };
        assert!(Config::from_request(&request).is_err(), "{bad}");
    }
    let both = r#""overrides": [{"db_type": "x", "column": "t.c", "python_type": "str"}]"#;
    let request = plugin::GenerateRequest {
        plugin_options: format!(r#"{{"backend": "sqlalchemy", "async": false, {both}}}"#)
            .into_bytes(),
        ..plugin::GenerateRequest::default()
    };
    let error = format!("{:#}", Config::from_request(&request).unwrap_err());
    assert!(
        error.contains("exactly one of db_type and column"),
        "{error}"
    );
}

#[test]
fn a_rename_names_a_class_or_a_field() {
    let schemas = vec![sql_schema(
        "public",
        vec![table(
            "media",
            vec![
                column("id", "pg_catalog.int8"),
                column("spotify_url", "text"),
            ],
        )],
        vec![],
    )];
    let api = resolve_options(
        schemas,
        &options(r#""rename": {"media": "Media", "spotify_url": "spotify_link"}"#),
    )
    .unwrap();
    assert_eq!(api.models[0].name, "Media");
    let names: Vec<&str> = api.models[0]
        .fields
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(names, ["id", "spotify_link"]);
}

#[test]
fn an_override_is_written_as_ruff_writes_it() {
    // A comment would otherwise swallow the ` | None` written after it.
    let config = options(r#""overrides": [{"db_type": "vector", "python_type": "app.T  # note"}]"#);
    assert_eq!(config.overrides[0].python_type, "app.T");
    let nullable = plugin::Column {
        not_null: false,
        ..column("v", "vector")
    };
    let schemas = public(vec![table("items", vec![nullable])]);
    let api = resolve_options(schemas, &config).unwrap();
    assert_eq!(annotations(&api), [("v".into(), "app.T | None".into())]);
}

#[test]
fn an_override_names_only_types_generated_code_can_import_as_written() {
    let error = |python_type: &str| {
        let request = plugin::GenerateRequest {
            plugin_options: format!(
                r#"{{"backend": "sqlalchemy", "async": false, "overrides": [{{"db_type": "x", "python_type": "{python_type}"}}]}}"#
            )
            .into_bytes(),
            ..plugin::GenerateRequest::default()
        };
        Config::from_request(&request)
            .err()
            .map(|e| format!("{e:#}"))
    };
    for (bad, problem) in [
        ("numpy.typing.NDArray[numpy.float64]", "ICN001"),
        ("pandas.Series", "ICN001"),
        ("datetime.date", "ICN001"),
        ("models.Account", "generated code binds"),
        ("sqlalchemy.Text", "generated code binds"),
        ("dt.date.max", "generated code binds"),
        ("Any", "neither a builtin"),
        ("Sequence[int]", "neither a builtin"),
    ] {
        let error = error(bad).unwrap_or_default();
        assert!(error.contains(problem), "{bad}: {error}");
    }
    for good in [
        "dt.date",
        "Decimal",
        "UUID",
        "list[app.T] | None",
        "IPv4Network",
    ] {
        assert_eq!(error(good), None, "{good}");
    }
}

#[test]
fn an_override_or_rename_that_matches_nothing_is_an_error() {
    let schemas = || public(vec![table("items", vec![column("id", "pg_catalog.int8")])]);
    for (json, problem) in [
        (
            r#""overrides": [{"db_type": "vectr", "python_type": "str"}]"#,
            "overrides[0]",
        ),
        (
            r#""overrides": [{"column": "item.id", "python_type": "str"}]"#,
            "overrides[0]",
        ),
        (r#""rename": {"itms": "Thing"}"#, "rename: \"itms\""),
    ] {
        let error = format!(
            "{:#}",
            resolve_options(schemas(), &options(json)).unwrap_err()
        );
        assert!(error.contains(problem), "{json}: {error}");
    }
}

#[test]
fn a_bare_table_column_override_is_the_default_schemas() {
    let v = plugin::Column {
        table: Some(plugin::Identifier {
            schema: "other".into(),
            name: "items".into(),
            ..plugin::Identifier::default()
        }),
        ..column("v", "vector")
    };
    let schemas = vec![sql_schema("other", vec![table("items", vec![v])], vec![])];
    let bare = options(r#""overrides": [{"column": "items.v", "python_type": "str"}]"#);
    assert!(resolve_options(schemas.clone(), &bare).is_err());
    let qualified = options(r#""overrides": [{"column": "other.items.v", "python_type": "str"}]"#);
    assert!(resolve_options(schemas, &qualified).is_ok());
}

#[test]
fn an_override_replaces_no_array_of_enums() {
    let mut moods = column("moods", "mood");
    moods.array_dims = 1;
    moods.is_array = true;
    let schemas = vec![sql_schema(
        "public",
        vec![table("people", vec![moods])],
        vec![plugin::Enum {
            name: "mood".into(),
            vals: vec!["happy".into()],
            ..Default::default()
        }],
    )];
    let config = options(r#""overrides": [{"db_type": "mood", "python_type": "str"}]"#);
    let error = format!("{:#}", resolve_options(schemas, &config).unwrap_err());
    assert!(error.contains("an override cannot replace them"), "{error}");
}

#[test]
fn a_class_is_capwords_and_hides_no_builtin() {
    let error = |schemas: Vec<plugin::Schema>, json: &str| {
        format!(
            "{:#}",
            resolve_options(schemas, &options(json)).unwrap_err()
        )
    };
    let warnings = || {
        public(vec![table(
            "warnings",
            vec![column("id", "pg_catalog.int8")],
        )])
    };
    assert!(error(warnings(), r#""rename": {}"#).contains("`Warning` is not a usable"));
    let renamed = error(warnings(), r#""rename": {"warnings": "warning_row"}"#);
    assert!(
        renamed.contains("`warning_row` is not a usable"),
        "{renamed}"
    );
}

#[test]
fn no_field_takes_the_name_of_a_module_an_override_imports() {
    let schemas = public(vec![table("items", vec![column("app", "vector")])]);
    let config = options(r#""overrides": [{"db_type": "vector", "python_type": "app.T"}]"#);
    let error = format!("{:#}", resolve_options(schemas, &config).unwrap_err());
    assert!(error.contains("`app` would hide the module"), "{error}");
}

#[test]
fn a_bare_db_type_override_names_the_default_schemas_type_too() {
    // sqlc reports an extension's type created in `public` as `public.vector`.
    let schemas = public(vec![table("items", vec![column("v", "public.vector")])]);
    let config = options(r#""overrides": [{"db_type": "vector", "python_type": "str"}]"#);
    let api = resolve_options(schemas, &config).unwrap();
    assert_eq!(annotations(&api), [("v".into(), "str".into())]);
}
