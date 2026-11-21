use super::*;
use crate::api::Api;
use crate::api::fixtures::catalog::*;
use crate::api::fixtures::*;
use crate::backend::Backend;

/// The options `{"backend": "sqlalchemy", "async": false, ...json}`.
fn options(json: &str) -> Config {
    let request = plugin::GenerateRequest {
        plugin_options: format!(r#"{{"backend": "sqlalchemy", "async": false, {json}}}"#).into_bytes(),
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
    let mut path = column("path", "vector", true);
    path.not_null = false;
    // sqlc names each catalog column's table.
    let label = plugin::Column {
        table: Some(plugin::Identifier {
            name: "items".into(),
            ..plugin::Identifier::default()
        }),
        ..column("label", "text", true)
    };
    let schemas = || {
        vec![sql_schema(
            "public",
            vec![table(
                "items",
                vec![column("id", "pg_catalog.int8", true), path.clone(), label.clone()],
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
fn a_rename_names_a_class_or_a_field() {
    let schemas = vec![sql_schema(
        "public",
        vec![table(
            "media",
            vec![
                column("id", "pg_catalog.int8", true),
                column("spotify_url", "text", true),
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
    let names: Vec<&str> = api.models[0].fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["id", "spotify_link"]);
}

#[test]
fn an_override_is_written_as_ruff_writes_it() {
    // A comment would otherwise swallow the ` | None` written after it.
    let config = options(r#""overrides": [{"db_type": "vector", "python_type": "app.T  # note"}]"#);
    let written = crate::python::unparse(&config.overrides[0].python_type.expr);
    assert_eq!(written, "app.T");
    let nullable = plugin::Column {
        not_null: false,
        ..column("v", "vector", true)
    };
    let schemas = public(vec![table("items", vec![nullable])]);
    let api = resolve_options(schemas, &config).unwrap();
    assert_eq!(annotations(&api), [("v".into(), "app.T | None".into())]);
}

#[test]
fn an_override_that_has_none_is_optional_once() {
    // A JSON column holds `null` whether or not it is nullable.
    let config = options(r#""overrides": [{"db_type": "jsonb", "python_type": "dict[str, int] | None"}]"#);
    let nullable = plugin::Column {
        not_null: false,
        ..column("maybe", "jsonb", true)
    };
    let schemas = public(vec![table("items", vec![column("always", "jsonb", true), nullable])]);
    let api = resolve_options(schemas, &config).unwrap();
    assert_eq!(
        annotations(&api),
        [
            ("always".into(), "dict[str, int] | None".into()),
            ("maybe".into(), "dict[str, int] | None".into())
        ]
    );
}

#[test]
fn an_override_or_rename_that_matches_nothing_is_an_error() {
    let schemas = || public(vec![table("items", vec![column("id", "pg_catalog.int8", true)])]);
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
        let error = format!("{:#}", resolve_options(schemas(), &options(json)).unwrap_err());
        assert!(error.contains(problem), "{json}: {error}");
    }
}

#[test]
fn an_override_of_a_type_the_schema_declares_may_go_unused_in_one_entry() {
    // Entries share one list of overrides, and only one of them queries with the composite
    let mut schemas = public(vec![table("items", vec![column("id", "pg_catalog.int8", true)])]);
    schemas[0].composite_types.push(plugin::CompositeType {
        name: "project_fqn".into(),
        comment: String::new(),
    });
    let shared = options(r#""overrides": [{"db_type": "project_fqn", "python_type": "Any"}]"#);
    resolve_options(schemas, &shared).unwrap();
}

#[test]
fn a_bare_table_column_override_is_the_default_schemas() {
    let v = plugin::Column {
        table: Some(plugin::Identifier {
            schema: "other".into(),
            name: "items".into(),
            ..plugin::Identifier::default()
        }),
        ..column("v", "vector", true)
    };
    let schemas = vec![sql_schema("other", vec![table("items", vec![v])], vec![])];
    let bare = options(r#""overrides": [{"column": "items.v", "python_type": "str"}]"#);
    assert!(resolve_options(schemas.clone(), &bare).is_err());
    let qualified = options(r#""overrides": [{"column": "other.items.v", "python_type": "str"}]"#);
    assert!(resolve_options(schemas, &qualified).is_ok());
}

#[test]
fn an_override_replaces_no_array_of_enums() {
    let mut moods = column("moods", "mood", true);
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
        format!("{:#}", resolve_options(schemas, &options(json)).unwrap_err())
    };
    let warnings = || public(vec![table("warnings", vec![column("id", "pg_catalog.int8", true)])]);
    assert!(error(warnings(), r#""rename": {}"#).contains("`Warning` is not a usable"));
    let renamed = error(warnings(), r#""rename": {"warnings": "warning_row"}"#);
    assert!(renamed.contains("`warning_row` is not a usable"), "{renamed}");
}

#[test]
fn no_field_takes_the_name_of_a_module_an_override_imports() {
    let schemas = public(vec![table("items", vec![column("app", "vector", true)])]);
    let config = options(r#""overrides": [{"db_type": "vector", "python_type": "app.T"}]"#);
    let error = format!("{:#}", resolve_options(schemas, &config).unwrap_err());
    assert!(error.contains("`app` would hide the module"), "{error}");
}

#[test]
fn a_rows_field_hides_an_overrides_module_as_a_models_does_and_a_lone_column_does_not() {
    use crate::api::fixtures::authors::{of_authors, query, try_resolve_with};
    let config = options(r#""overrides": [{"column": "authors.name", "python_type": "author.T"}]"#);
    let resolve = |columns: Vec<plugin::Column>, text: &str| {
        let query = plugin::Query {
            columns,
            ..query(":many", text)
        };
        try_resolve_with("postgresql", vec![query], &config)
    };
    let embed = plugin::Column {
        name: "authors".into(),
        embed_table: Some(plugin::Identifier {
            name: "authors".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    // The embed's field is `author`, beside one annotated `author.T`.
    let name = || of_authors(column("name", "text", true));
    let error = error_of(resolve(
        vec![embed, name()],
        "SELECT authors.id, authors.name, authors.bio, authors.name FROM authors",
    ));
    assert!(error.contains("`author` would hide the module"), "{error}");
    // A lone column is no field of a class.
    let lone = plugin::Column {
        name: "author".into(),
        original_name: "name".into(),
        ..name()
    };
    resolve(vec![lone], "SELECT name AS author FROM authors").unwrap();
}

#[test]
fn a_bare_db_type_override_names_the_default_schemas_type_too() {
    // sqlc reports an extension's type created in `public` as `public.vector`.
    let schemas = public(vec![table("items", vec![column("v", "public.vector", true)])]);
    let config = options(r#""overrides": [{"db_type": "vector", "python_type": "str"}]"#);
    let api = resolve_options(schemas, &config).unwrap();
    assert_eq!(annotations(&api), [("v".into(), "str".into())]);
}

#[test]
fn a_rename_of_a_tables_singular_is_no_rename_of_the_table() {
    // `author` names a column; the table `authors` keeps its class.
    let schemas = public(vec![table(
        "authors",
        vec![column("id", "pg_catalog.int8", true), column("author", "text", true)],
    )]);
    let api = resolve_options(schemas, &options(r#""rename": {"author": "writer"}"#)).unwrap();
    assert_eq!(api.models[0].name, "Author");
    let fields: Vec<&str> = api.models[0].fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(fields, ["id", "writer"]);
}

#[test]
fn a_composite_the_schema_declares_is_never_the_type_of_its_name() {
    // A `data.json` of the user's is no JSON
    let mut data = sql_schema("data", vec![], vec![]);
    data.composite_types.push(plugin::CompositeType {
        name: "json".into(),
        comment: String::new(),
    });
    let mut schemas = public(vec![table("events", vec![column("payload", "data.json", true)])]);
    schemas.push(data);
    let error = format!(
        "{:#}",
        resolve_options(schemas, &Config::of(Backend::SqlAlchemy)).unwrap_err()
    );
    assert!(
        error.contains("\"data.json\", which has no verified Python mapping"),
        "{error}"
    );
}

#[test]
fn a_bare_override_names_an_extensions_type_in_any_schema_but_no_declared_one() {
    let schemas = || {
        public(vec![table(
            "items",
            vec![column("embedding", "extensions.vector", true)],
        )])
    };
    let bare = options(r#""overrides": [{"db_type": "vector", "python_type": "str"}]"#);
    let api = resolve_options(schemas(), &bare).unwrap();
    assert_eq!(annotations(&api), [("embedding".into(), "str".into())]);

    let mood = plugin::Enum {
        name: "mood".into(),
        vals: vec!["happy".into()],
        comment: String::new(),
    };
    let mut declared = public(vec![table("people", vec![column("feeling", "data.mood", true)])]);
    declared.push(sql_schema("data", vec![], vec![mood]));
    let bare = options(r#""overrides": [{"db_type": "mood", "python_type": "str"}]"#);
    assert!(resolve_options(declared, &bare).is_err());

    // Nor PostgreSQL's own type a domain of another schema is named after, which sqlc does not declare
    let domain = || public(vec![table("prices", vec![column("amount", "archive.money", true)])]);
    let bare = options(r#""overrides": [{"db_type": "money", "python_type": "str"}]"#);
    assert!(resolve_options(domain(), &bare).is_err());
    let qualified = options(r#""overrides": [{"db_type": "archive.money", "python_type": "Decimal"}]"#);
    let api = resolve_options(domain(), &qualified).unwrap();
    assert_eq!(annotations(&api), [("amount".into(), "Decimal".into())]);
}

#[test]
fn unknown_type_annotates_every_type_we_cannot_verify_as_asked() {
    let mut tags = column("tags", "citext", true);
    (tags.is_array, tags.array_dims) = (true, 1);
    let schemas = || {
        let columns = vec![
            column("shape", "geometry", true),
            column("area", "geometry", false),
            tags.clone(),
        ];
        public(vec![table("plots", columns)])
    };
    assert!(resolve_options(schemas(), &options(r#""max_params": null"#)).is_err());

    // The whole column, as a driver may return an array of it as its text
    let api = resolve_options(schemas(), &options(r#""unknown_type": "Any""#)).unwrap();
    let any = [("shape", "Any"), ("area", "Any | None"), ("tags", "Any")];
    assert_eq!(annotations(&api), any.map(|(name, ty)| (name.into(), ty.into())));
    let api = resolve_options(schemas(), &options(r#""unknown_type": "object""#)).unwrap();
    let object = [("shape", "object"), ("area", "object | None"), ("tags", "object")];
    assert_eq!(annotations(&api), object.map(|(name, ty)| (name.into(), ty.into())));
}

#[test]
fn unknown_type_annotates_a_type_sqlc_guessed_too() {
    use crate::api::fixtures::authors::{query, try_resolve_with};
    use crate::api::queries::{Command, Shape};
    let guessed = plugin::Query {
        columns: vec![column("total", "int", true)],
        ..query(":one", "SELECT a + b AS total FROM t")
    };
    let modules = try_resolve_with("postgresql", vec![guessed], &options(r#""unknown_type": "Any""#)).unwrap();
    let Command::One(Shape::Scalar(ty)) = &modules[0].queries[0].command else {
        panic!("a scalar :one");
    };
    assert_eq!(ty.describe(crate::python::Scope::Queries), "Any");
}

#[test]
fn unknown_type_is_any_or_object_and_leaves_other_errors_alone() {
    let request = |value: &str| plugin::GenerateRequest {
        plugin_options: format!(r#"{{"backend": "sqlalchemy", "async": false, "unknown_type": "{value}"}}"#)
            .into_bytes(),
        ..plugin::GenerateRequest::default()
    };
    for wrong in ["any", "Object", "str"] {
        assert!(Config::from_request(&request(wrong)).is_err(), "{wrong}");
    }

    // sqlc contradicting itself is no type to accept
    let mut flat = column("tags", "text", true);
    flat.is_array = true;
    let schemas = public(vec![table("plots", vec![flat])]);
    let error = format!(
        "{:#}",
        resolve_options(schemas, &options(r#""unknown_type": "Any""#)).unwrap_err()
    );
    assert!(error.contains("sqlc reported no array depth"), "{error}");
}

#[test]
fn a_type_override_reaches_every_spelling_of_its_type() {
    // sqlc reports a `bigint` column as `pg_catalog.int8` and a `bigserial` one as `bigserial`
    let schemas = || {
        public(vec![table(
            "items",
            vec![column("id", "bigserial", true), column("big", "pg_catalog.int8", true)],
        )])
    };
    for db_type in ["int8", "bigint", "pg_catalog.int8", "bigserial"] {
        let config = options(&format!(
            r#""overrides": [{{"db_type": "{db_type}", "python_type": "app.Id"}}]"#
        ));
        let api = resolve_options(schemas(), &config).unwrap();
        let expected = [("id", "app.Id"), ("big", "app.Id")];
        assert_eq!(
            annotations(&api),
            expected.map(|(name, ty)| (name.into(), ty.into())),
            "{db_type}"
        );
    }
}

#[test]
fn a_type_override_never_reaches_a_type_sqlc_guessed() {
    use crate::api::fixtures::authors::{query, try_resolve_with};
    // sqlc types `sum(amount)` of a numeric column `bigint`, where the driver returns a `Decimal`
    let sum = |is_func_call: bool| plugin::Query {
        columns: vec![plugin::Column {
            is_func_call,
            ..column("total", "bigint", false)
        }],
        ..query(":one", "SELECT sum(amount) AS total FROM authors")
    };
    let config = options(r#""overrides": [{"db_type": "bigint", "python_type": "int"}]"#);
    for is_func_call in [true, false] {
        let error = error_of(try_resolve_with("postgresql", vec![sum(is_func_call)], &config));
        assert!(error.contains("add a cast"), "{error}");
    }
}

#[test]
fn a_column_override_types_a_column_sqlc_could_not() {
    use crate::api::fixtures::authors::{of_authors, query, try_resolve_with};
    use crate::api::queries::{Command, Shape};
    // SQLite takes a column declared without a type, which sqlc reports as `any`
    let untyped = plugin::Query {
        columns: vec![plugin::Column {
            original_name: "bio".into(),
            ..of_authors(column("bio", "any", true))
        }],
        ..query(":one", "SELECT bio FROM authors")
    };
    let config = options(r#""overrides": [{"column": "authors.bio", "python_type": "str"}]"#);
    let modules = try_resolve_with("sqlite", vec![untyped], &config).unwrap();
    let Command::One(Shape::Scalar(ty)) = &modules[0].queries[0].command else {
        panic!("a scalar :one");
    };
    assert_eq!(ty.describe(crate::python::Scope::Queries), "str");
}

#[test]
fn a_type_override_never_reaches_a_parameter_sqlc_typed_itself() {
    use crate::api::fixtures::authors::{of_authors, param, query, try_resolve_with};
    // sqlc types a `LIMIT`'s parameter `integer` on every engine, with no column of the type behind it
    for (engine, db_type, data_type, text) in [
        (
            "postgresql",
            "int4",
            "pg_catalog.int4",
            "SELECT 1 FROM authors WHERE age = $1 LIMIT $2",
        ),
        (
            "sqlite",
            "integer",
            "INTEGER",
            "SELECT 1 FROM authors WHERE age = ?1 LIMIT ?2",
        ),
    ] {
        let paginated = plugin::Query {
            params: vec![
                param(1, of_authors(column("age", data_type, true))),
                param(2, column("limit", "integer", true)),
            ],
            ..query(":exec", text)
        };
        let config = options(&format!(
            r#""overrides": [{{"db_type": "{db_type}", "python_type": "app.Age"}}]"#
        ));
        let modules = try_resolve_with(engine, vec![paginated], &config).unwrap();
        let types: Vec<String> = (modules[0].queries[0].params.iter())
            .map(|parameter| parameter.ty.describe(crate::python::Scope::Queries))
            .collect();
        assert_eq!(types, ["app.Age", "int"], "{engine}");
    }
}
