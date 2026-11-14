//! Specification for overrides and renames.

use super::*;
use crate::api::Api;
use crate::api::fixtures::catalog::*;
use crate::api::fixtures::*;

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
                vec![
                    column("id", "pg_catalog.int8", true),
                    path.clone(),
                    label.clone(),
                ],
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
    let config =
        options(r#""overrides": [{"db_type": "jsonb", "python_type": "dict[str, int] | None"}]"#);
    let nullable = plugin::Column {
        not_null: false,
        ..column("maybe", "jsonb", true)
    };
    let schemas = public(vec![table(
        "items",
        vec![column("always", "jsonb", true), nullable],
    )]);
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
        ("datetime.date", "write `dt.date`"),
        ("int, str", "is no type"),
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
        "tuple[float, ...]",
        "IPv4Network",
    ] {
        assert_eq!(error(good), None, "{good}");
    }
}

#[test]
fn an_override_or_rename_that_matches_nothing_is_an_error() {
    let schemas = || {
        public(vec![table(
            "items",
            vec![column("id", "pg_catalog.int8", true)],
        )])
    };
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
        format!(
            "{:#}",
            resolve_options(schemas, &options(json)).unwrap_err()
        )
    };
    let warnings = || {
        public(vec![table(
            "warnings",
            vec![column("id", "pg_catalog.int8", true)],
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
    let schemas = public(vec![table("items", vec![column("app", "vector", true)])]);
    let config = options(r#""overrides": [{"db_type": "vector", "python_type": "app.T"}]"#);
    let error = format!("{:#}", resolve_options(schemas, &config).unwrap_err());
    assert!(error.contains("`app` would hide the module"), "{error}");
}

#[test]
fn a_bare_db_type_override_names_the_default_schemas_type_too() {
    // sqlc reports an extension's type created in `public` as `public.vector`.
    let schemas = public(vec![table(
        "items",
        vec![column("v", "public.vector", true)],
    )]);
    let config = options(r#""overrides": [{"db_type": "vector", "python_type": "str"}]"#);
    let api = resolve_options(schemas, &config).unwrap();
    assert_eq!(annotations(&api), [("v".into(), "str".into())]);
}

#[test]
fn a_rename_of_a_tables_singular_is_no_rename_of_the_table() {
    // `author` names a column; the table `authors` keeps its class.
    let schemas = public(vec![table(
        "authors",
        vec![
            column("id", "pg_catalog.int8", true),
            column("author", "text", true),
        ],
    )]);
    let api = resolve_options(schemas, &options(r#""rename": {"author": "writer"}"#)).unwrap();
    assert_eq!(api.models[0].name, "Author");
    let fields: Vec<&str> = api.models[0]
        .fields
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(fields, ["id", "writer"]);
}
