//! Specification for resolving a catalog into classes and annotations.

use super::*;
use crate::config::{Config, ModelConfig};
use crate::python::Imports;
use ruff_python_codegen::{Generator, Indentation};
use ruff_source_file::LineEnding;

// -- Annotations -------------------------------------------------------------

fn field(element: Type, array_dims: usize, optional: bool) -> FieldType {
    FieldType {
        element,
        array_dims,
        optional,
    }
}

fn annotation(ty: &FieldType) -> String {
    render(&ty.annotation(Scope::Models))
}

fn render(expr: &ast::Expr) -> String {
    Generator::new(&Indentation::default(), LineEnding::Lf).expr(expr)
}

#[test]
fn a_plain_column_annotates_as_its_element_type() {
    assert_eq!(annotation(&field(Type::INT, 0, false)), "int");
}

#[test]
fn a_nullable_column_is_optional() {
    assert_eq!(annotation(&field(Type::STR, 0, true)), "Optional[str]");
}

#[test]
fn each_array_dimension_adds_a_list() {
    assert_eq!(annotation(&field(Type::INT, 1, false)), "List[int]");
    assert_eq!(
        annotation(&field(Type::INT, 3, false)),
        "List[List[List[int]]]"
    );
}

#[test]
fn a_nullable_array_is_an_optional_list_not_a_list_of_optionals() {
    // The column may be absent; its elements are not independently nullable.
    assert_eq!(
        annotation(&field(Type::INT, 1, true)),
        "Optional[List[int]]"
    );
}

#[test]
fn a_generated_element_is_written_for_the_scope_it_appears_in() {
    let ty = field(Type::Generated("Status".into()), 0, true);
    let scope = Scope::Queries {
        models_module: "models",
    };
    assert_eq!(render(&ty.annotation(scope)), "Optional[models.Status]");
}

#[test]
fn the_imports_are_exactly_what_the_annotation_mentions() {
    let imports = |ty: FieldType| ty.imports().collect::<Vec<_>>();

    assert_eq!(imports(field(Type::INT, 0, false)), []);
    assert_eq!(
        imports(field(Type::DATE, 0, true)),
        [
            Import::module("datetime"),
            Import::from("typing", "Optional")
        ]
    );
    assert_eq!(
        imports(field(Type::INT, 1, true)),
        [
            Import::from("typing", "List"),
            Import::from("typing", "Optional")
        ]
    );
}

#[test]
fn a_multidimensional_array_still_only_imports_list_once() {
    let collected: Imports = field(Type::INT, 3, false).imports().collect();
    let statements: Vec<String> = collected
        .statements()
        .iter()
        .map(|stmt| Generator::new(&Indentation::default(), LineEnding::Lf).stmt(stmt))
        .collect();

    assert_eq!(statements, ["from typing import List"]);
}

// -- Resolving a catalog -----------------------------------------------------

fn resolve(schemas: Vec<plugin::Schema>, config: &Config) -> Schema {
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
    Schema::resolve(&request, config).unwrap()
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

fn model_names(schema: &Schema) -> Vec<&str> {
    schema.models.iter().map(|m| m.name.as_str()).collect()
}

#[test]
fn a_table_becomes_a_singular_class_name() {
    let resolved = resolve(
        public(vec![
            table("authors", vec![column("id", "int8")]),
            table("test_scores", vec![column("score", "int4")]),
        ]),
        &Config::default(),
    );

    assert_eq!(model_names(&resolved), ["Author", "TestScore"]);
}

#[test]
fn exact_table_names_keeps_the_table_name_verbatim() {
    let config = Config {
        models: ModelConfig {
            exact_table_names: true,
            ..ModelConfig::default()
        },
        ..Config::default()
    };
    let resolved = resolve(public(vec![table("authors", vec![])]), &config);

    assert_eq!(model_names(&resolved), ["Authors"]);
}

#[test]
fn a_table_outside_the_default_schema_is_qualified() {
    // Two `events` tables in one catalog must not collapse to one class.
    let resolved = resolve(
        vec![
            sql_schema("public", vec![table("events", vec![])], vec![]),
            sql_schema("analytics", vec![table("events", vec![])], vec![]),
        ],
        &Config::default(),
    );

    assert_eq!(model_names(&resolved), ["AnalyticsEvent", "Event"]);
}

#[test]
fn the_engines_own_schemas_are_not_modelled() {
    let resolved = resolve(
        vec![
            sql_schema("public", vec![table("authors", vec![])], vec![]),
            sql_schema("pg_catalog", vec![table("pg_class", vec![])], vec![]),
        ],
        &Config::default(),
    );

    assert_eq!(model_names(&resolved), ["Author"]);
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
        &Config::default(),
    );

    assert_eq!(
        resolved.models[0].fields[0].ty.element,
        Type::Generated("JobStatus".into())
    );
    assert_eq!(resolved.enums[0].name, "JobStatus");

    let members: Vec<&str> = resolved.enums[0]
        .members
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    assert_eq!(members, ["QUEUED", "IN_PROGRESS"]);
}

#[test]
fn a_type_nothing_declares_falls_back_to_any() {
    let resolved = resolve(
        public(vec![table("odd", vec![column("value", "some_extension")])]),
        &Config::default(),
    );

    assert_eq!(resolved.models[0].fields[0].ty.element, Type::ANY);
}

#[test]
fn a_column_that_admits_null_is_optional() {
    let nullable = plugin::Column {
        not_null: false,
        ..column("bio", "text")
    };
    let resolved = resolve(
        public(vec![table("authors", vec![nullable])]),
        &Config::default(),
    );

    assert!(resolved.models[0].fields[0].ty.optional);
}

#[test]
fn array_depth_is_read_from_whichever_signal_the_catalog_sets() {
    // Older catalogs set only the flag; newer ones also set a depth.
    let flag_only = plugin::Column {
        is_array: true,
        ..column("tags", "text")
    };
    let with_depth = plugin::Column {
        is_array: true,
        array_dims: 2,
        ..column("grid", "int4")
    };
    let resolved = resolve(
        public(vec![table("things", vec![flag_only, with_depth])]),
        &Config::default(),
    );

    assert_eq!(resolved.models[0].fields[0].ty.array_dims, 1);
    assert_eq!(resolved.models[0].fields[1].ty.array_dims, 2);
}

#[test]
fn an_unsupported_engine_is_rejected() {
    let request = plugin::GenerateRequest {
        settings: Some(plugin::Settings {
            engine: "oracle".into(),
            ..plugin::Settings::default()
        }),
        ..plugin::GenerateRequest::default()
    };

    let error = Schema::resolve(&request, &Config::default()).unwrap_err();
    assert!(error.to_string().contains("oracle"), "{error}");
}

#[test]
fn a_model_recognises_its_own_table() {
    let resolved = resolve(public(vec![table("authors", vec![])]), &Config::default());
    let unqualified = plugin::Identifier {
        name: "authors".into(),
        ..plugin::Identifier::default()
    };

    // A column's table reference often omits the schema, meaning the default.
    assert!(resolved.models[0].is(&unqualified, "public"));
    assert!(!resolved.models[0].is(&unqualified, "analytics"));
}

// -- Singularisation ---------------------------------------------------------

#[test]
fn singularises_the_table_names_a_schema_actually_uses() {
    for (plural, expected) in [
        ("authors", "author"),
        ("addresses", "address"),
        ("statuses", "status"),
        ("categories", "category"),
        ("people", "person"),
        ("children", "child"),
        ("matrices", "matrix"),
        ("indices", "index"),
        ("mice", "mouse"),
        ("analyses", "analysis"),
        ("leaves", "leaf"),
        ("buses", "bus"),
        ("cities", "city"),
        // Already singular, or uncountable.
        ("series", "series"),
        ("species", "species"),
        ("equipment", "equipment"),
        ("news", "news"),
        ("campus", "campus"),
        ("meta", "meta"),
    ] {
        assert_eq!(singular(plural), expected, "singular({plural})");
    }
}
