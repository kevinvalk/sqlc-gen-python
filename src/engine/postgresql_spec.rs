//! Specification for the PostgreSQL type mapping.

use super::*;

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
    // `numeric` and `money` are exact; a float would silently lose precision.
    assert_eq!(scalar_type("numeric"), Some(Type::DECIMAL));
    assert_eq!(scalar_type("money"), Some(Type::DECIMAL));
}

#[test]
fn every_integer_width_is_one_python_int() {
    for width in [
        "smallint",
        "integer",
        "bigint",
        "serial",
        "bigserial",
        "oid",
    ] {
        assert_eq!(scalar_type(width), Some(Type::INT), "{width}");
    }
}

#[test]
fn the_date_and_time_types_are_distinguished() {
    assert_eq!(scalar_type("date"), Some(Type::DATE));
    assert_eq!(scalar_type("time"), Some(Type::TIME));
    assert_eq!(scalar_type("timestamptz"), Some(Type::DATETIME));
    assert_eq!(scalar_type("interval"), Some(Type::TIMEDELTA));
}

#[test]
fn a_type_the_schema_declares_is_left_unresolved() {
    // An enum or composite type is not a scalar; `schema` resolves it.
    assert_eq!(scalar_type("mood"), None);
}
