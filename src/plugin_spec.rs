//! Specification for the sqlc wire-protocol helpers.

use super::*;

fn identifier(schema: &str, name: &str) -> Identifier {
    Identifier {
        schema: schema.into(),
        name: name.into(),
        ..Identifier::default()
    }
}

#[test]
fn a_type_name_is_qualified_only_when_the_schema_says_so() {
    assert_eq!(
        identifier("pg_catalog", "int8").data_type(),
        "pg_catalog.int8"
    );
    assert_eq!(identifier("", "int8").data_type(), "int8");
}

#[test]
fn a_column_without_a_type_reports_an_empty_one() {
    // sqlc leaves the type unset for columns it could not resolve.
    assert_eq!(Column::default().data_type(), "");
}

#[test]
fn an_unqualified_reference_means_the_default_schema() {
    let public = identifier("public", "authors");
    let unqualified = identifier("", "authors");

    assert!(unqualified.is(&public, "public"));
    assert!(!unqualified.is(&public, "analytics"));
}

#[test]
fn the_engines_own_schemas_are_never_generated_for() {
    for internal in INTERNAL_SCHEMAS {
        let schema = Schema {
            name: internal.into(),
            ..Schema::default()
        };
        assert!(schema.is_internal(), "{internal}");
    }

    let public = Schema {
        name: "public".into(),
        ..Schema::default()
    };
    assert!(!public.is_internal());
}

#[test]
fn only_user_schemas_are_walked() {
    let catalog = Catalog {
        schemas: ["public", "pg_catalog", "analytics", "information_schema"]
            .map(|name| Schema {
                name: name.into(),
                ..Schema::default()
            })
            .to_vec(),
        ..Catalog::default()
    };

    let names: Vec<&str> = catalog.user_schemas().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["public", "analytics"]);
}

#[test]
fn names_outside_the_default_schema_are_disambiguated() {
    let analytics = Schema {
        name: "analytics".into(),
        ..Schema::default()
    };
    let public = Schema {
        name: "public".into(),
        ..Schema::default()
    };

    // Two `events` tables in one catalog must not produce one class name.
    assert_eq!(public.qualify("events", "public"), "events");
    assert_eq!(analytics.qualify("events", "public"), "analytics_events");
}
