//! Specification for the SQLite type mapping.

use super::*;

#[test]
fn the_integer_affinity_spellings_all_map_to_int() {
    for spelling in ["int", "integer", "bigint", "unsignedbigint", "int2"] {
        assert_eq!(scalar_type(spelling), Some(Type::INT), "{spelling}");
    }
}

#[test]
fn the_text_affinity_spellings_all_map_to_str() {
    for spelling in ["text", "clob", "varchar", "nvarchar", "varyingcharacter"] {
        assert_eq!(scalar_type(spelling), Some(Type::STR), "{spelling}");
    }
}

#[test]
fn declared_numerics_keep_their_precision() {
    assert_eq!(scalar_type("decimal"), Some(Type::DECIMAL));
    assert_eq!(scalar_type("real"), Some(Type::FLOAT));
}

#[test]
fn an_unknown_name_is_left_unresolved() {
    assert_eq!(scalar_type("mood"), None);
}
