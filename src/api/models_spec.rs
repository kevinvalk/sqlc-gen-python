use anyhow::Result;

use super::*;
use crate::api::fixtures::catalog::{resolve, *};
use crate::api::fixtures::*;

fn field(element: Type, array_dims: usize, optional: bool) -> FieldType {
    FieldType {
        element,
        array_dims,
        optional,
        as_text: false,
    }
}

fn annotation(ty: &FieldType) -> String {
    ty.describe(Scope::Models)
}

#[test]
fn a_plain_column_annotates_as_its_element_type() {
    assert_eq!(annotation(&field(Type::INT, 0, false)), "int");
    assert_eq!(annotation(&field(Type::DATE, 0, false)), "dt.date");
    assert_eq!(annotation(&field(Type::CIDR, 0, false)), "IPv4Network | IPv6Network");
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
    assert_eq!(annotation(&field(Type::INT, 3, false)), "list[list[list[int]]]");
}

#[test]
fn a_nullable_array_is_an_optional_list_not_a_list_of_optionals() {
    // The column may be absent; its elements are not independently nullable.
    assert_eq!(annotation(&field(Type::INT, 1, true)), "list[int] | None");
}

#[test]
fn a_table_becomes_a_singular_class_name() {
    let resolved = resolve(
        public(vec![
            table("authors", vec![column("id", "int8", true)]),
            table("test_scores", vec![column("score", "int4", true)]),
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

    assert_eq!(model_names(&resolve(tables(), &config)), ["Author", "Media"]);
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
        vec![sql_schema("public", vec![], vec![named("zone"), named("area")])],
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
            vec![table("jobs", vec![column("status", "job_status", true)])],
            vec![plugin::Enum {
                name: "job_status".into(),
                vals: vec!["queued".into(), "in-progress".into()],
                comment: String::new(),
            }],
        )],
        &ModelConfig::default(),
    );

    assert_eq!(resolved.models[0].fields[0].ty.element, Type::Enum("JobStatus".into()));
    assert_eq!(resolved.enums[0].name, "JobStatus");

    let members: Vec<&str> = resolved.enums[0].members.iter().map(|m| m.name.as_str()).collect();
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
        ..column("method", data_type, true)
    };
    let resolved = try_resolve(vec![
        sql_schema("public", vec![table("payments", vec![column])], vec![payment_method()]),
        sql_schema("billing", vec![], vec![payment_method()]),
    ])?;
    Ok(resolved.models[0].fields[0].ty.clone())
}

#[test]
fn an_enum_is_found_in_the_schema_the_column_names_or_else_the_default_one() {
    // Two schemas may declare one name.
    let class = |data_type| enum_column(data_type, 0).unwrap().element;
    assert_eq!(class("payment_method"), Type::Enum("PaymentMethod".into()));
    assert_eq!(class("public.payment_method"), Type::Enum("PaymentMethod".into()));
    assert_eq!(
        class("billing.payment_method"),
        Type::Enum("BillingPaymentMethod".into())
    );
    assert!(enum_column("sales.payment_method", 0).is_err());
}

#[test]
fn an_enum_the_schema_declares_wins_over_a_type_of_postgresqls_with_its_name() {
    let mood = plugin::Enum {
        name: "money".into(),
        vals: vec!["rich".into()],
        comment: String::new(),
    };
    let resolved = try_resolve(vec![sql_schema(
        "public",
        vec![table("people", vec![column("mood", "public.money", true)])],
        vec![mood],
    )])
    .unwrap();
    assert_eq!(resolved.models[0].fields[0].ty.element, Type::Enum("Money".into()));
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
    // Both drivers return labels as `str`.
    let row = || crate::python::name("row").index(crate::python::int(3));
    let read = |ty: FieldType, scope| crate::python::unparse(&ty.read(row(), scope));
    let status = || Type::Enum("Status".into());
    assert_eq!(read(field(Type::DECIMAL, 1, true), Scope::Models), "row[3]");
    assert_eq!(read(field(status(), 0, false), Scope::Models), "Status(row[3])");
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
    let as_text = FieldType {
        as_text: true,
        ..field(status(), 1, false)
    };
    assert_eq!(read(as_text, Scope::Queries), "models.Status.from_array(row[3])");
}

#[test]
fn a_type_nothing_declares_is_an_error_naming_the_column() {
    // AGENTS.md: never paper over an unknown with `Any` unasked. A domain is one too: sqlc sends its name, never its base
    // type, so the error says what to declare instead, or how to accept it unknown.
    let error = try_resolve(public(vec![table("odd", vec![column("value", "amount", true)])])).unwrap_err();

    assert_eq!(
        format!("{error:#}"),
        r#"table "odd": column "value" has type "amount", which has no verified Python mapping; name its Python type with an override (db_type "amount"), or, for a domain, declare the column with its base type and a CHECK constraint; or set unknown_type: Any to leave it unchecked"#
    );
}

#[test]
fn an_array_the_drivers_do_not_load_as_a_list_is_an_error() {
    // `citext` is a `str`, but psycopg returns a `citext[]` as its text, and
    // asyncpg corrupts an empty item of a `tsvector[]`.
    for data_type in ["citext", "tsvector"] {
        let mut tags = column("tags", data_type, true);
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
        let view = table("v_totals", vec![column("total", guess, true)]);
        let error = try_resolve(public(vec![view])).unwrap_err();
        assert!(
            format!("{error:#}")
                .starts_with(r#"table "v_totals": column "total" has a type sqlc could not infer; add a cast"#),
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
            vec![table("paths", vec![column("start", "point_type", true)])],
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
        ..column("bio", "text", true)
    };
    let resolved = resolve(public(vec![table("authors", vec![nullable])]), &ModelConfig::default());

    assert!(resolved.models[0].fields[0].ty.optional);
}

#[test]
fn an_array_sqlc_gave_no_depth_is_an_error_not_a_guess() {
    // sqlc 1.31 sets a depth on every array column; one without is a request
    // this plugin has no evidence of.
    let tags = plugin::Column {
        is_array: true,
        ..column("tags", "text", true)
    };
    let error = try_resolve(public(vec![table("posts", vec![tags])])).unwrap_err();
    assert_eq!(
        format!("{error:#}"),
        r#"table "posts": column "tags" is an array, but sqlc reported no array depth for it"#
    );
}

#[test]
fn an_array_deeper_than_an_array_can_be_is_an_error() {
    // The depth is the request's: `-1` or a billion is no `list` to nest.
    for array_dims in [-1, 7, i32::MAX] {
        let tags = plugin::Column {
            is_array: true,
            array_dims,
            ..column("tags", "text", true)
        };
        let error = try_resolve(public(vec![table("posts", vec![tags])])).unwrap_err();
        assert!(
            format!("{error:#}").contains("where an array has six at most"),
            "{error:#}"
        );
    }
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
        "`Sequence` is a name generated code imports; name the table or type otherwise with the \
         rename option"
    );
    let decimal = plugin::Enum {
        name: "decimal".into(),
        ..plugin::Enum::default()
    };
    let error = try_resolve(vec![sql_schema("public", vec![], vec![decimal])]).unwrap_err();
    assert!(error.to_string().starts_with("`Decimal` is a name"), "{error}");
}

#[test]
fn omit_unused_writes_the_models_and_enums_the_queries_use_and_reports_no_others_problems() {
    use crate::api::fixtures::authors::{authors, named, of_authors, query, selecting};
    use crate::backend::Backend;
    use crate::config::Config;

    let enum_of = |name: &str| plugin::Enum {
        name: name.into(),
        vals: vec!["one".into()],
        ..plugin::Enum::default()
    };
    let schemas = vec![sql_schema(
        "public",
        vec![
            table("authors", authors()),
            table("books", vec![column("id", "int8", true)]),
            table("labels", vec![column("id", "int8", true)]),
            // An unknown type, which is no problem of a table no query uses
            table("reviews", vec![column("body", "amount", true)]),
        ],
        vec![enum_of("mood"), enum_of("colour")],
    )];
    let list_authors = plugin::Query {
        name: "ListAuthors".into(),
        text: selecting(&authors()),
        columns: authors().into_iter().map(of_authors).collect(),
        ..query(":many", "")
    };
    let books = plugin::Column {
        name: "books".into(),
        embed_table: Some(plugin::Identifier {
            name: "books".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let get_book = plugin::Query {
        name: "GetBook".into(),
        columns: vec![books, column("total", "int8", true)],
        ..query(":one", "SELECT books.id, t.total FROM books JOIN t ON true")
    };
    let set_mood = plugin::Query {
        name: "SetMood".into(),
        params: vec![named(1, "mood", "mood", true)],
        ..query(":exec", "UPDATE authors SET bio = $1")
    };
    let request = request("postgresql", schemas, vec![list_authors, get_book, set_mood]);
    let config = |omit_unused| Config {
        models: ModelConfig {
            omit_unused,
            ..ModelConfig::default()
        },
        ..Config::of(Backend::SqlAlchemy)
    };

    let api = crate::api::resolve(&request, &config(true)).unwrap();
    assert_eq!(model_names(&api), ["Author", "Book"]);
    assert_eq!(api.enums.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["Mood"]);

    let error = error_of(crate::api::resolve(&request, &config(false)));
    assert!(error.contains(r#"table "reviews""#), "{error}");
}
