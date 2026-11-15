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

/// The fixture the runtime proof reads back through every driver
/// (`tests/python/typemap_sqlite.py`).
#[test]
fn every_rule_decides_a_column_the_runtime_proof_reads() {
    // Where cargo runs the test from, not where it was built (a reused
    // `target/` may come from another copy of the tree).
    let path = std::path::Path::new(&std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("tests/sqlc/examples/sqlitetypes.json");
    let request: crate::plugin::GenerateRequest =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let declared: Vec<String> = (request.catalog.iter())
        .flat_map(|catalog| &catalog.schemas)
        .flat_map(|schema| &schema.tables)
        .flat_map(|table| &table.columns)
        .map(|column| column.data_type().to_ascii_uppercase())
        .collect();

    let decides = |word: &str| {
        (declared.iter()).any(|name| affinity(name).is_some_and(|(rule, _)| *rule == word))
    };
    let unread: Vec<&str> = (AFFINITIES.iter().map(|(word, _)| *word))
        .filter(|word| !decides(word))
        .chain((BOOLEANS.iter().copied()).filter(|name| !declared.iter().any(|d| d == name)))
        .collect();
    assert!(unread.is_empty(), "no column of {unread:?}");
}
