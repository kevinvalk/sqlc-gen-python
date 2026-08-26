//! Specification for the plugin configuration surface.

use super::*;

fn parse(json: &str) -> Result<Config> {
    let request = plugin::GenerateRequest {
        plugin_options: json.as_bytes().to_vec(),
        settings: Some(plugin::Settings {
            codegen: Some(plugin::Codegen {
                out: "src/authors".into(),
                ..plugin::Codegen::default()
            }),
            ..plugin::Settings::default()
        }),
        ..plugin::GenerateRequest::default()
    };
    Config::from_request(&request)
}

#[test]
fn defaults_the_package_to_the_output_directory() {
    let options = parse("{}").unwrap();
    assert_eq!(options.package, "authors");
    assert_eq!(options.models.file, "models.py");
    assert!(options.models.emit);
    assert!(!options.asynchronous);
}

#[test]
fn reads_nested_groups() {
    let options = parse(
        r#"{
            "package": "db",
            "async": true,
            "models": { "style": "pydantic", "exact_table_names": true },
            "queries": { "style": "module", "many": "list" },
            "enums": { "style": "str_enum" }
        }"#,
    )
    .unwrap();

    assert_eq!(options.package, "db");
    assert!(options.asynchronous);
    assert_eq!(options.models.style, ClassStyle::Pydantic);
    assert!(options.models.exact_table_names);
    assert_eq!(options.queries.style, QuerierStyle::Module);
    assert_eq!(options.queries.many, ManyStyle::List);
    assert_eq!(options.enums.style, EnumStyle::StrEnum);
}

#[test]
fn rejects_unknown_keys() {
    // The old `emit_*` flag soup is gone; a stale config should say so
    // rather than being quietly ignored.
    let error = parse(r#"{"emit_sync_querier": true}"#).unwrap_err();
    assert!(error.to_string().contains("plugin options"), "{error}");
}

#[test]
fn rejects_generating_nothing() {
    let error = parse(r#"{"models": {"emit": false}, "queries": {"emit": false}}"#).unwrap_err();
    assert!(error.to_string().contains("nothing would be generated"));
}

#[test]
fn derives_the_models_module_from_the_filename() {
    assert_eq!(parse("{}").unwrap().models_module(), "models");
    assert_eq!(
        parse(r#"{"models": {"file": "schema.py"}}"#)
            .unwrap()
            .models_module(),
        "schema"
    );
}

#[test]
fn excludes_tables_from_inflection() {
    let options = parse(r#"{"models": {"inflection_exclude": ["Campus"]}}"#).unwrap();
    assert!(options.models.is_inflection_excluded("campus"));
    assert!(!options.models.is_inflection_excluded("authors"));
}
