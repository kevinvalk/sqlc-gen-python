//! Specification for the MySQL type mapping.

use super::*;

#[test]
fn signedness_does_not_change_the_python_type() {
    // Python integers are arbitrary precision, so width and sign are the
    // database's concern alone.
    for spelling in ["bigint", "bigint signed", "bigint unsigned", "tinyint"] {
        assert_eq!(scalar_type(spelling), Some(Type::INT), "{spelling}");
    }
}

#[test]
fn time_is_a_duration_not_a_wall_clock() {
    // MySQL's TIME is signed and may exceed 24 hours, so it is not
    // `datetime.time` — unlike every other engine here.
    assert_eq!(scalar_type("time"), Some(Type::TIMEDELTA));
    assert_eq!(scalar_type("datetime"), Some(Type::DATETIME));
}

#[test]
fn the_text_and_blob_families_split_by_content_not_size() {
    for text in ["char", "varchar", "tinytext", "longtext"] {
        assert_eq!(scalar_type(text), Some(Type::STR), "{text}");
    }
    for blob in ["binary", "tinyblob", "longblob"] {
        assert_eq!(scalar_type(blob), Some(Type::BYTES), "{blob}");
    }
}

#[test]
fn an_unknown_name_is_left_unresolved() {
    assert_eq!(scalar_type("mood"), None);
}
