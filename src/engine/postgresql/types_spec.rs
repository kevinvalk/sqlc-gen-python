//! Specification for the PostgreSQL type mapping.

use std::collections::BTreeSet;

use super::*;
use crate::plugin;
use crate::python::Scope;

#[test]
fn a_type_reads_the_same_bare_or_pg_catalog_qualified() {
    for (bare, qualified) in [
        ("int8", "pg_catalog.int8"),
        ("bool", "pg_catalog.bool"),
        ("numeric", "pg_catalog.numeric"),
        ("timestamptz", "pg_catalog.timestamptz"),
    ] {
        assert_eq!(scalar_type(bare), scalar_type(qualified), "{bare}");
    }
}

#[test]
fn exact_numerics_do_not_become_floats() {
    assert_eq!(scalar_type("numeric"), Some(Type::DECIMAL));
}

#[test]
fn money_is_the_text_the_drivers_return() {
    // psycopg and asyncpg both return `'$12.34'`: annotating it `Decimal`
    // would be a lie the type checkers cannot see through.
    assert_eq!(scalar_type("money"), Some(Type::STR));
}

#[test]
fn json_is_object_because_its_decoder_is_configurable() {
    assert_eq!(Type::JSON.source(Scope::Models), "object");
    assert_eq!(Type::JSONB.source(Scope::Queries), "object");
}

#[test]
fn json_is_told_from_jsonb_whatever_the_spelling() {
    assert_eq!(scalar_type("json"), Some(Type::JSON));
    assert_eq!(scalar_type("pg_catalog.jsonb"), Some(Type::JSONB));
}

#[test]
fn network_types_are_what_the_drivers_return() {
    assert_eq!(scalar_type("inet"), Some(Type::INET));
    assert_eq!(scalar_type("cidr"), Some(Type::CIDR));
}

#[test]
fn a_declared_or_cast_type_reads_as_the_type_it_names() {
    // A serial column arrives as its pseudo-type, and a LIMIT or OFFSET
    // parameter as sqlc's own `integer`.
    for (spelling, typname) in [
        ("smallserial", "int2"),
        ("serial", "int4"),
        ("integer", "int4"),
        ("bigserial", "int8"),
    ] {
        assert_eq!(scalar_type(spelling), scalar_type(typname), "{spelling}");
        assert!(!is_guess(spelling, false), "{spelling}");
    }
}

#[test]
fn sqls_standard_names_are_guesses() {
    // sqlc's static analyser writes a declared or cast type as PostgreSQL's
    // own name (`pg_catalog.int8`, `timestamptz`), and SQL's standard name
    // only for a type it read off a function's signature: the overload it
    // guessed. So is a bare `int`, its report of any arithmetic expression,
    // and a type it has no name for.
    for guess in [
        "",
        "any",
        "anyelement",
        "anyarray",
        "int",
        "smallint",
        "bigint",
        "real",
        "double precision",
        "numeric",
        "boolean",
        "character",
        "character varying",
        "interval",
        "time with time zone",
        "time without time zone",
        "timestamp with time zone",
        "timestamp without time zone",
    ] {
        assert!(is_guess(guess, false), "{guess:?}");
    }
    for declared in [
        "pg_catalog.int8",
        "pg_catalog.numeric",
        "pg_catalog.interval",
        "pg_catalog.timestamptz",
        "int8",
        "timestamptz",
        "text",
        "date",
        "integer",
    ] {
        assert!(!is_guess(declared, false), "{declared}");
    }
}

#[test]
fn an_array_a_driver_does_not_load_as_a_list_says_what_it_does() {
    // psycopg returns `ARRAY['x'::citext]` as the text `'{x}'`, and asyncpg
    // decodes `ARRAY[''::tsvector]` as `[<NULL>]` (docs/typemap.md).
    assert_eq!(scalar_type("citext"), Some(Type::STR));
    assert!(array_problem("citext").unwrap().contains("psycopg"));
    assert!(
        array_problem("pg_catalog.tsvector")
            .unwrap()
            .contains("asyncpg")
    );
    assert_eq!(array_problem("pg_catalog.text"), None);
}

/// The fixture the runtime proof reads back through every driver
/// (`tests/python/typemap.py`).
#[test]
fn every_mapped_type_has_a_column_the_runtime_proof_reads() {
    // Where cargo runs the test from, not where it was built (a reused
    // `target/` may come from another copy of the tree).
    let path = std::path::Path::new(&std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("tests/sqlc/examples/pgtypes.json");
    let request: plugin::GenerateRequest =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let columns = || {
        (request.catalog.iter())
            .flat_map(|catalog| &catalog.schemas)
            .flat_map(|schema| &schema.tables)
            .flat_map(|table| &table.columns)
    };
    for array in [false, true] {
        let probed: BTreeSet<String> = (columns().filter(|column| column.is_array == array))
            .map(|column| typname(&column.data_type()).to_owned())
            .collect();
        let missing: Vec<&str> = (TYPES.iter().map(|(name, _)| *name))
            .filter(|name| !(array && array_problem(name).is_some()) && !probed.contains(*name))
            .collect();
        assert!(
            missing.is_empty(),
            "no column (array: {array}) of {missing:?}"
        );
    }
}

#[test]
fn a_type_without_driver_evidence_is_unknown() {
    // The drivers disagree on these, or nobody has probed them, so the schema
    // reports them as errors rather than guessing.
    for unprobed in [
        "int4range",
        "point",
        "pg_catalog.bit",
        "vector",
        "tid",
        "any",
        // A type the schema declares, such as an enum, is the schema's to
        // resolve.
        "mood",
    ] {
        assert_eq!(scalar_type(unprobed), None, "{unprobed}");
    }
}

#[test]
fn every_integer_width_is_one_python_int() {
    for width in ["int2", "int4", "pg_catalog.int8", "serial", "oid"] {
        assert_eq!(scalar_type(width), Some(Type::INT), "{width}");
    }
}

#[test]
fn the_date_and_time_types_are_distinguished() {
    assert_eq!(scalar_type("date"), Some(Type::DATE));
    assert_eq!(scalar_type("time"), Some(Type::TIME));
    assert_eq!(scalar_type("timestamptz"), Some(Type::DATETIME));
    assert_eq!(scalar_type("pg_catalog.interval"), Some(Type::TIMEDELTA));
}

#[test]
fn analysed_on_a_database_a_standard_name_is_the_type_postgresql_resolved() {
    // `format_type`'s spelling, its modifiers left out.
    for (spelling, python) in [
        ("integer", Type::INT),
        ("bigint", Type::INT),
        ("boolean", Type::BOOL),
        ("double precision", Type::FLOAT),
        ("numeric(12,2)", Type::DECIMAL),
        ("character varying(255)", Type::STR),
        ("timestamp(3) with time zone", Type::DATETIME),
    ] {
        assert!(!is_guess(spelling, true), "{spelling}");
        assert_eq!(scalar_type(spelling), Some(python), "{spelling}");
    }
    assert!(is_guess("bigint", false));
    assert!(is_guess("any", true));
}
