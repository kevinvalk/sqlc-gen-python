//! Specification for SQLite's type map.

use super::*;

#[test]
fn a_declared_type_maps_by_the_class_sqlite_stores_it_as() {
    for (declared, python) in [
        ("INTEGER", Type::INT),
        ("bigint", Type::INT),
        ("REAL", Type::FLOAT),
        ("DOUBLEPRECISION", Type::FLOAT),
        ("TEXT", Type::STR),
        ("VARCHAR(32)", Type::STR),
        ("NVARCHAR(32)", Type::STR),
        ("CLOB", Type::STR),
        ("blob", Type::BYTES),
        ("BOOLEAN", Type::BOOL_FROM_INT),
        ("DATETIME", Type::STR),
        ("json", Type::STR),
    ] {
        assert_eq!(scalar_type(declared), Some(python), "{declared}");
    }
}

#[test]
fn a_type_whose_values_sqlite_may_round_is_not_mapped() {
    for declared in ["NUMERIC", "DECIMAL(10,5)", "money", ""] {
        assert_eq!(scalar_type(declared), None, "{declared}");
    }
}

#[test]
fn any_and_nothing_are_guesses() {
    for guess in ["any", ""] {
        assert!(is_guess(guess));
    }
    assert!(!is_guess("integer"));
}
