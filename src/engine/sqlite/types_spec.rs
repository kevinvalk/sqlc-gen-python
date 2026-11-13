//! Specification for SQLite's type map.

use super::*;

#[test]
fn a_declared_type_maps_by_the_class_its_affinity_stores() {
    for (declared, python) in [
        ("INTEGER", Type::INT),
        ("bigint", Type::INT),
        // SQLite's rules, in their order: `INT` first, wherever it is.
        ("FLOATING POINT", Type::INT),
        ("REAL", Type::FLOAT),
        ("DOUBLEPRECISION", Type::FLOAT),
        ("float", Type::FLOAT),
        ("TEXT", Type::STR),
        ("VARCHAR(32)", Type::STR),
        ("NVARCHAR(32)", Type::STR),
        ("CLOB", Type::STR),
        ("blob", Type::BYTES),
        ("BOOLEAN", Type::BOOL_FROM_INT),
    ] {
        assert_eq!(scalar_type(declared), Some(python), "{declared}");
    }
}

#[test]
fn a_type_of_numeric_affinity_is_not_mapped() {
    // Each stores `'2024'` as an integer and `'12.50'` as a real.
    for declared in [
        "NUMERIC",
        "DECIMAL(10,5)",
        "DATE",
        "DATETIME",
        "JSON",
        "money",
        "",
    ] {
        assert_eq!(scalar_type(declared), None, "{declared}");
    }
}
