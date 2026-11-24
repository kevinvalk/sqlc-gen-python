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
fn an_extension_type_reads_the_same_in_whichever_schema_it_was_created_in() {
    // `pg_dump` writes the schema out, and an extension may be created in any (`CREATE EXTENSION ... WITH SCHEMA`)
    for qualified in ["public.citext", "extensions.citext", "data.citext"] {
        assert_eq!(scalar_type(qualified), Some(Type::STR), "{qualified}");
        assert_eq!(
            array_problem(qualified, false, false),
            array_problem("citext", false, false),
            "{qualified}"
        );
    }
}

#[test]
fn a_type_of_another_schema_named_like_postgresqls_own_is_a_domain_and_unknown() {
    // sqlc reports a domain by its name, never its base type: `archive.money` may well be a numeric
    for qualified in ["archive.money", "public.money", "data.timestamp", "archive.name"] {
        assert_eq!(scalar_type(qualified), None, "{qualified}");
    }
    assert_eq!(scalar_type("pg_catalog.money"), Some(Type::STR));
}

#[test]
fn exact_numerics_do_not_become_floats() {
    assert_eq!(scalar_type("numeric"), Some(Type::DECIMAL));
}

#[test]
fn money_is_the_text_the_drivers_return() {
    // psycopg and asyncpg both return `'$12.34'`, so annotating it `Decimal` would be a lie the type checkers cannot
    // see through!
    assert_eq!(scalar_type("money"), Some(Type::STR));
}

#[test]
fn json_is_object_because_its_decoder_is_configurable() {
    let source = |r#type: Type, scope| crate::python::unparse(&r#type.annotation(scope));
    assert_eq!(source(Type::JSON, Scope::Models), "object");
    assert_eq!(source(Type::JSONB, Scope::Queries), "object");
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
    // A serial column arrives as its pseudo-type, and a LIMIT or OFFSET parameter as sqlc's own `integer`
    for (spelling, typname) in [
        ("smallserial", "int2"),
        ("serial", "int4"),
        ("integer", "int4"),
        ("bigserial", "int8"),
    ] {
        assert_eq!(scalar_type(spelling), scalar_type(typname), "{spelling}");
    }
}

#[test]
fn an_array_a_driver_does_not_load_as_a_list_says_what_it_does() {
    // psycopg returns `ARRAY['a=>b'::hstore]` as its text, registered or not, and asyncpg decodes `ARRAY[''::tsvector]`
    // as `[<NULL>]` (`docs/typemap.md`)
    assert!(array_problem("hstore", false, false).unwrap().contains("psycopg"));
    assert!(
        array_problem("pg_catalog.tsvector", false, false)
            .unwrap()
            .contains("asyncpg")
    );
    assert_eq!(array_problem("pg_catalog.text", false, false), None);
}

#[test]
fn every_type_but_postgresqls_own_is_registered_before_its_array_is_read() {
    // psycopg returns `ARRAY['x'::citext]` as the text `'{x}'` until the type is registered on the connection, as it knows
    // the OID of PostgreSQL's own types alone
    for name in [
        "citext",
        "public.citext",
        "extensions.ltree",
        "lquery",
        "hstore",
        "mood",
        "billing.payment_method",
    ] {
        assert!(is_registered(name), "{name}");
    }
    for name in [
        "text",
        "pg_catalog.int4",
        "integer",
        "jsonb",
        "timestamp with time zone",
    ] {
        assert!(!is_registered(name), "{name}");
    }
    for name in ["citext", "ltree", "lquery"] {
        assert_eq!(array_problem(name, false, false), None, "{name}");
        // asyncpg counts an empty item of one, and never returns it
        assert!(array_problem(name, false, true).unwrap().contains("asyncpg"), "{name}");
    }
}

#[test]
fn an_array_a_driver_does_not_read_as_a_list_still_binds_as_a_parameter() {
    // Whichever driver binds it, asyncpg included
    for name in ["citext", "public.citext", "lquery", "ltree", "tsvector"] {
        assert_eq!(array_problem(name, true, true), None, "{name}");
    }
    // A list of dicts, and names of relations a value must name, which nobody has probed
    assert!(array_problem("hstore", true, false).is_some());
    assert!(array_problem("regclass", true, false).is_some());
    // `ltree` has no operator that takes a list of them, so nobody has probed one
    assert!(array_problem("ltxtquery", true, false).is_some());
}

/// The runtime proof (`tests/python/typemap.py`) reads this fixture back through every driver.
#[test]
fn every_mapped_type_has_a_column_the_runtime_proof_reads() {
    // We read from where cargo runs the test and not where it was built, as a reused `target/` may come from another
    // copy of the tree
    let path =
        std::path::Path::new(&std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("tests/sqlc/examples/pgtypes.json");
    let request: plugin::GenerateRequest = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let columns = || {
        (request.catalog.iter())
            .flat_map(|catalog| &catalog.schemas)
            .flat_map(|schema| &schema.tables)
            .flat_map(|table| &table.columns)
    };
    for array in [false, true] {
        let probed: BTreeSet<String> = (columns().filter(|column| column.is_array == array))
            .map(|column| type_name(&column.data_type()).to_owned())
            .collect();
        let missing: Vec<&str> = (TYPES.iter().map(|(name, _)| *name))
            .filter(|name| !(array && array_problem(name, false, false).is_some()) && !probed.contains(*name))
            .collect();
        assert!(missing.is_empty(), "no column (array: {array}) of {missing:?}");
    }
}

#[test]
fn a_type_without_driver_evidence_is_unknown() {
    // The drivers disagree on these or nobody has probed them, so the schema reports them as errors instead of
    // guessing
    for unprobed in [
        "int4range",
        "point",
        "pg_catalog.bit",
        "vector",
        "tid",
        "any",
        // A type the schema declares, such as an enum, is the schema's to resolve
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
