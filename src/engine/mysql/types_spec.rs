//! Specification for MySQL's type map.

use super::*;

fn column(name: &str, length: i32) -> plugin::Column {
    plugin::Column {
        r#type: Some(plugin::Identifier {
            name: name.into(),
            ..Default::default()
        }),
        length,
        ..Default::default()
    }
}

#[test]
fn a_type_maps_to_what_pymysql_and_asyncmy_return() {
    for (name, length, python) in [
        ("int", -1, Type::INT),
        ("bigint unsigned", -1, Type::INT),
        ("tinyint", -1, Type::INT),
        ("year", -1, Type::INT),
        ("double", -1, Type::FLOAT),
        ("decimal", 10, Type::DECIMAL),
        ("varchar", 255, Type::STR),
        ("longtext", -1, Type::STR),
        ("json", -1, Type::STR),
        ("varbinary", 32, Type::BYTES),
        ("bit", 1, Type::BYTES),
        ("date", -1, Type::DATE),
        ("datetime", 19, Type::DATETIME),
        ("timestamp", 19, Type::DATETIME),
        ("time", 10, Type::TIMEDELTA),
    ] {
        assert_eq!(scalar_type(&column(name, length)), Some(python), "{name}");
    }
}

#[test]
fn a_boolean_is_tinyint_1_converted() {
    for (name, length) in [("bool", -1), ("boolean", -1), ("tinyint", 1)] {
        assert_eq!(
            scalar_type(&column(name, length)),
            Some(Type::BOOL_FROM_INT),
            "{name}"
        );
    }
}

#[test]
fn a_type_nobody_probed_is_not_mapped() {
    for name in ["geometry", "point", "any", ""] {
        assert_eq!(scalar_type(&column(name, -1)), None, "{name}");
    }
}

#[test]
fn a_set_is_told_from_an_enum_by_its_length() {
    let labels = ["one".to_string(), "two".into(), "three".into()];
    assert!(enum_problem(&column("t_cset", 13), &labels).is_some());
    assert!(enum_problem(&column("t_cenum", 5), &labels).is_none());
}

#[test]
fn a_set_is_measured_in_bytes_and_one_member_is_ambiguous() {
    let labels = ["é".to_string(), "ü".into()];
    // Each label two bytes: a SET's length is 2 + 1 + 2.
    assert!(enum_problem(&column("t_cset", 5), &labels).is_some());
    assert!(enum_problem(&column("t_cenum", 2), &labels).is_none());
    let error = enum_problem(&column("t_one", 1), &["a".to_string()]).unwrap();
    assert!(error.contains("one member"), "{error}");
}
