//! Specification for the sqlc wire-protocol helpers.

use super::*;

fn schema(name: &str) -> Schema {
    Schema {
        name: name.into(),
        ..Schema::default()
    }
}

fn catalog(default_schema: &str) -> Catalog {
    Catalog {
        default_schema: default_schema.into(),
        schemas: ["public", "pg_catalog", "analytics", "information_schema"]
            .map(schema)
            .to_vec(),
        ..Catalog::default()
    }
}

#[test]
fn an_unqualified_reference_means_the_default_schema() {
    let public = Identifier {
        schema: "public".into(),
        name: "authors".into(),
        ..Identifier::default()
    };
    let unqualified = Identifier {
        schema: String::new(),
        ..public.clone()
    };

    assert!(unqualified.is(&public, &catalog("public")));
    assert!(!unqualified.is(&public, &catalog("analytics")));
    assert_eq!(
        catalog("public").schema("").map(|s| s.name.as_str()),
        Some("public")
    );
}

#[test]
fn the_engines_own_schemas_are_never_walked() {
    let catalog = catalog("public");

    let names: Vec<&str> = catalog.user_schemas().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["public", "analytics"]);
    assert!(catalog.schema("pg_catalog").is_none());
}

#[test]
fn sqlc_sends_a_plugin_no_overrides_or_renames_of_its_own() {
    // So this plugin's `overrides` and `rename` options are the only ones; a
    // sqlc that sends sqlc-gen-go's must be supported before it is pinned.
    let proto = include_str!("../protos/sqlc.proto");
    let settings = &proto[proto.find("message Settings {").unwrap()..];
    let fields: Vec<&str> = (settings.lines().skip(1))
        .take_while(|line| *line != "}")
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect();
    for field in ["rename", "overrides"] {
        assert!(
            !fields
                .iter()
                .any(|line| line.contains(&format!(" {field} ="))),
            "sqlc now sends {field} to plugins"
        );
    }
}
